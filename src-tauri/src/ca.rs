use base64::Engine as _;
use base64::engine::general_purpose;
use once_cell::sync::Lazy;
use rcgen::{
    BasicConstraints, Certificate, CertificateParams, DistinguishedName, DnType,
    ExtendedKeyUsagePurpose, IsCa, KeyPair, KeyUsagePurpose,
};
use regex::Regex;
use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

static CA_DIR: Lazy<PathBuf> = Lazy::new(|| {
    let mut p = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    p.push("mitm-ca");
    p
});

fn ca_cert_path() -> PathBuf {
    let mut p = CA_DIR.clone();
    p.push("rootCA.pem");
    p
}
fn ca_key_path() -> PathBuf {
    let mut p = CA_DIR.clone();
    p.push("rootCA.key.pem");
    p
}

pub fn ensure_ca_exists() -> Result<(String, String), String> {
    fs::create_dir_all(&*CA_DIR).ok();
    let cert_path = ca_cert_path();
    let key_path = ca_key_path();
    if cert_path.exists() && key_path.exists() {
        let cert_pem = fs::read_to_string(&cert_path).map_err(|e| e.to_string())?;
        let key_pem = fs::read_to_string(&key_path).map_err(|e| e.to_string())?;
        return Ok((cert_pem, key_pem));
    }
    let mut params = CertificateParams::new(vec![]);
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.not_before = rcgen::date_time_ymd(2020, 1, 1);
    params.not_after = rcgen::date_time_ymd(2045, 1, 1);
    // 明确声明为 CA 的 KeyUsage
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, "PromptDumper Root CA");
    params.distinguished_name = dn;
    // 使用 ECDSA P-256（rcgen 默认支持生成），更利于测试环境稳定
    let key_pair = KeyPair::generate(&rcgen::PKCS_ECDSA_P256_SHA256).map_err(|e| e.to_string())?;
    params.key_pair = Some(key_pair);
    let ca = Certificate::from_params(params).map_err(|e| e.to_string())?;
    let cert_pem = ca.serialize_pem().map_err(|e| e.to_string())?;
    let key_pem = ca.serialize_private_key_pem();
    fs::write(&cert_path, &cert_pem).map_err(|e| e.to_string())?;
    fs::write(&key_path, &key_pem).map_err(|e| e.to_string())?;
    Ok((cert_pem, key_pem))
}

#[cfg(target_os = "macos")]
pub fn install_ca_to_system_trust(cert_pem: &str) -> Result<(), String> {
    // 优先方案：生成并尝试以命令行静默安装 .mobileconfig（仅弹一次管理员密码），失败再打开系统设置
    if let Ok(()) = generate_and_open_mobileconfig(cert_pem) {
        return Ok(());
    }

    // 回退：使用持久化的 CA 证书路径，尝试直接写入系统钥匙串
    let path = ca_cert_path();
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    if existing != cert_pem {
        let mut f = std::fs::File::create(&path).map_err(|e| e.to_string())?;
        f.write_all(cert_pem.as_bytes())
            .map_err(|e| e.to_string())?;
    }
    // 优先以提权方式执行（触发管理员密码弹窗），并在必要时先解锁系统钥匙串
    let system_keychain = "/Library/Keychains/System.keychain";
    let sh_cmd = format!(
        "/usr/bin/security unlock-keychain -d system '{}' && /usr/bin/security add-trusted-cert -d -r trustRoot -p ssl -k '{}' '{}'",
        system_keychain,
        system_keychain,
        path.display()
    );
    let osa_script = format!(
        "do shell script \"{}\" with administrator privileges with prompt \"PromptDumper 需要管理员权限以安装系统信任证书\"",
        sh_cmd.replace("\\", "\\\\").replace("\"", "\\\"")
    );
    if let Ok(st) = Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(&osa_script)
        .status()
    {
        if st.success() {
            return Ok(());
        }
    }

    // 直接尝试（当已有足够权限时可成功）
    let status = Command::new("/usr/bin/security")
        .arg("add-trusted-cert")
        .arg("-d")
        .arg("-r")
        .arg("trustRoot")
        .arg("-p")
        .arg("ssl")
        .arg("-k")
        .arg(system_keychain)
        .arg(&path)
        .status()
        .map_err(|e| e.to_string())?;
    if status.success() {
        return Ok(());
    }

    // 兜底：打开证书文件，提示手动导入
    let _ = Command::new("/usr/bin/open").arg(&path).status();
    Err(format!(
        "自动安装到系统钥匙串失败。你可以在终端手动执行:\n  sudo security add-trusted-cert -d -r trustRoot -p ssl -k /Library/Keychains/System.keychain '{}'",
        path.display()
    ))
}

#[cfg(target_os = "macos")]
fn generate_and_open_mobileconfig(cert_pem: &str) -> Result<(), String> {
    // 将 PEM 转 DER 并 base64，放入 PayloadContent
    let der = pem_to_der_first_cert(cert_pem)?;
    let b64 = general_purpose::STANDARD.encode(&der);
    let payload_uuid = Uuid::new_v4().to_string().to_uppercase();
    let profile_uuid = Uuid::new_v4().to_string().to_uppercase();
    // 构造有效的 XML Plist 配置文件（.mobileconfig 必须是 plist 而非 JSON）
    let profile = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>PayloadContent</key>
  <array>
    <dict>
      <key>PayloadCertificateFileName</key>
      <string>PromptDumperRootCA.cer</string>
      <key>PayloadContent</key>
      <data>
{b64}
      </data>
      <key>PayloadDescription</key>
      <string>Installs PromptDumper Root CA</string>
      <key>PayloadDisplayName</key>
      <string>PromptDumper Root CA</string>
      <key>PayloadIdentifier</key>
      <string>com.promptdumper.ca</string>
      <key>PayloadType</key>
      <string>com.apple.security.root</string>
      <key>PayloadUUID</key>
      <string>{payload_uuid}</string>
      <key>PayloadVersion</key>
      <integer>1</integer>
    </dict>
  </array>
  <key>PayloadDescription</key>
  <string>Install root CA for PromptDumper</string>
  <key>PayloadDisplayName</key>
  <string>PromptDumper Root CA</string>
  <key>PayloadIdentifier</key>
  <string>com.promptdumper.profile</string>
  <key>PayloadOrganization</key>
  <string>PromptDumper</string>
  <key>PayloadRemovalDisallowed</key>
  <false/>
  <key>PayloadType</key>
  <string>Configuration</string>
  <key>PayloadUUID</key>
  <string>{profile_uuid}</string>
  <key>PayloadVersion</key>
  <integer>1</integer>
 </dict>
</plist>
"#
    );
    let tmp = tempfile::NamedTempFile::new().map_err(|e| e.to_string())?;
    let path = tmp.path().with_extension("mobileconfig");
    std::fs::write(&path, profile).map_err(|e| e.to_string())?;
    // 首选：使用 profiles CLI 直接安装（需要管理员权限，但无需点击系统设置）
    let sh_cmd = format!(
        "/usr/bin/profiles install -type configuration -path '{}'",
        path.display()
    );
    let osa_script = format!(
        "do shell script \"{}\" with administrator privileges with prompt \"PromptDumper 需要安装根证书到系统信任\"",
        sh_cmd.replace("\\", "\\\\").replace("\"", "\\\"")
    );
    if let Ok(st) = Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(&osa_script)
        .status()
    {
        if st.success() {
            return Ok(());
        }
    }
    // 回退：打开系统设置到配置文件页面（尝试用户域和系统域）
    let _ = Command::new("/usr/bin/open")
        .arg("x-apple.systempreferences:com.apple.preferences.configurationprofiles")
        .status();
    let _ = Command::new("/usr/bin/open").arg(&path).status();
    Err("需要在系统设置中点击安装已下载的描述文件".into())
}

#[cfg(not(target_os = "macos"))]
pub fn install_ca_to_system_trust(_cert_pem: &str) -> Result<(), String> {
    Ok(())
}

pub fn generate_leaf_cert_for_host(
    host: &str,
    _ca_cert_pem: &str,
    ca_key_pem: &str,
) -> Result<(Vec<u8>, Vec<u8>, Vec<u8>), String> {
    let ca_key = rcgen::KeyPair::from_pem(ca_key_pem).map_err(|e| e.to_string())?;
    let mut ca_params = CertificateParams::new(vec![]);
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, "PromptDumper Root CA");
    ca_params.distinguished_name = dn;
    ca_params.key_pair = Some(ca_key);
    let ca_cert = Certificate::from_params(ca_params).map_err(|e| e.to_string())?;

    let mut leaf_params = CertificateParams::new(vec![host.to_string()]);
    // Keep validity windows short (Apple clients reject >398d lifetimes).
    let now = OffsetDateTime::now_utc();
    leaf_params.not_before = now.saturating_sub(Duration::days(1));
    leaf_params.not_after = leaf_params
        .not_before
        .checked_add(Duration::days(397))
        .ok_or_else(|| "failed to compute certificate validity".to_string())?;
    leaf_params
        .distinguished_name
        .push(DnType::CommonName, host);
    // 使用 ECDSA P-256（rcgen 支持生成），更稳定
    leaf_params.alg = &rcgen::PKCS_ECDSA_P256_SHA256;
    // 明确声明为服务器证书用途
    leaf_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    // 对 ECDSA，digitalSignature 足够
    leaf_params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    let leaf = Certificate::from_params(leaf_params).map_err(|e| e.to_string())?;
    let cert_der = leaf
        .serialize_der_with_signer(&ca_cert)
        .map_err(|e| e.to_string())?;
    let key_der = leaf.serialize_private_key_der();
    let ca_der = ca_cert.serialize_der().map_err(|e| e.to_string())?;
    Ok((cert_der, key_der, ca_der))
}

/// 将 PEM 字符串中的首个 CERTIFICATE 块解码为 DER
pub fn pem_to_der_first_cert(pem: &str) -> Result<Vec<u8>, String> {
    let begin = "-----BEGIN CERTIFICATE-----";
    let end = "-----END CERTIFICATE-----";
    let bpos = pem
        .find(begin)
        .ok_or_else(|| "invalid pem: missing begin".to_string())?;
    let rest = &pem[bpos + begin.len()..];
    let epos_rel = rest
        .find(end)
        .ok_or_else(|| "invalid pem: missing end".to_string())?;
    let b64 = rest[..epos_rel]
        .lines()
        .map(|l| l.trim())
        .collect::<String>();
    let der = general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| e.to_string())?;
    Ok(der)
}

#[cfg(target_os = "macos")]
pub fn is_ca_installed_in_system_trust() -> Result<bool, String> {
    // 在 user/system 两个域的钥匙串搜索证书
    let re = Regex::new(r"SHA-(?:256|1) hash:\s*[0-9A-F]{40,64}").map_err(|e| e.to_string())?;
    let mut kc_set: HashSet<String> = HashSet::new();
    for domain in ["user", "system"].iter() {
        if let Ok(out) = Command::new("/usr/bin/security")
            .arg("list-keychains")
            .arg("-d")
            .arg(domain)
            .output()
        {
            let s = String::from_utf8_lossy(&out.stdout);
            for line in s.lines() {
                if let Some(start) = line.find('"') {
                    if let Some(end) = line[start + 1..].find('"') {
                        kc_set.insert(line[start + 1..start + 1 + end].to_string());
                    }
                }
            }
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        kc_set.insert(format!("{}/Library/Keychains/login.keychain-db", home));
    }
    kc_set.insert("/Library/Keychains/System.keychain".to_string());
    kc_set.insert("/System/Library/Keychains/SystemRootCertificates.keychain".to_string());

    for kc in kc_set.iter() {
        if let Ok(out) = Command::new("/usr/bin/security")
            .arg("find-certificate")
            .arg("-a")
            .arg("-Z")
            .arg("-c")
            .arg("PromptDumper Root CA")
            .arg(kc)
            .output()
        {
            let s = String::from_utf8_lossy(&out.stdout).to_string();
            if re.is_match(&s) {
                return Ok(true);
            }
            // 兼容个别系统只返回 PEM 的情况：尝试解析是否包含 BEGIN CERTIFICATE
            if s.contains("-----BEGIN CERTIFICATE-----") {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

#[cfg(not(target_os = "macos"))]
pub fn is_ca_installed_in_system_trust() -> Result<bool, String> {
    Ok(false)
}

#[cfg(target_os = "macos")]
pub fn uninstall_ca_from_system_trust() -> Result<(), String> {
    // Detect user cancellation from various system messages
    let user_cancelled = |s: &str| -> bool {
        let l = s.to_lowercase();
        l.contains("用户已取消")
            || l.contains("user canceled")
            || l.contains("the authorization was canceled by the user")
            || l.contains("authorization was canceled by the user")
            || l.contains("(-128)")
    };
    // 优先尝试通过 profiles 移除配置文件（同时覆盖 system/user、多种 CLI 语法）
    let rm_cmd = "sh -c 'U=$(/usr/bin/stat -f %Su /dev/console); \
      /usr/bin/profiles remove -identifier com.promptdumper.profile || true; \
      /usr/bin/profiles remove -type configuration -identifier com.promptdumper.profile || true; \
      /usr/bin/profiles -R -p com.promptdumper.profile || true; \
      /usr/bin/profiles remove -identifier com.promptdumper.profile -user \"$U\" || true; \
      /usr/bin/profiles -R -p com.promptdumper.profile -user \"$U\" || true'";
    let osa_script = format!(
        "do shell script \"{}\" with administrator privileges",
        rm_cmd.replace("\\", "\\\\").replace("\"", "\\\"")
    );
    if let Ok(out) = Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(&osa_script)
        .output()
    {
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        if user_cancelled(&combined) {
            return Err("操作已取消：用户在授权对话框中点击了取消".into());
        }
        // 无论成功与否，继续尝试清理钥匙串证书（除非用户取消）
    }
    // First, remove trust settings using the actual PEM(s) from keychain
    // This avoids cases where deletion fails due to trust records
    if let Ok(out_pem) = Command::new("/usr/bin/security")
        .arg("find-certificate")
        .arg("-a")
        .arg("-p")
        .arg("-c")
        .arg("PromptDumper Root CA")
        .output()
    {
        if out_pem.status.success() {
            let text = String::from_utf8_lossy(&out_pem.stdout);
            let mut buf = String::new();
            let mut in_pem = false;
            for line in text.lines() {
                if line.contains("-----BEGIN CERTIFICATE-----") {
                    in_pem = true;
                    buf.clear();
                    buf.push_str(line);
                    buf.push('\n');
                    continue;
                }
                if in_pem {
                    buf.push_str(line);
                    buf.push('\n');
                    if line.contains("-----END CERTIFICATE-----") {
                        // write to temp file
                        if let Ok(tmp) = tempfile::NamedTempFile::new() {
                            let path = tmp.path().to_path_buf();
                            let _ = std::fs::write(&path, &buf);
                            // Remove from admin domain
                            if let Ok(out1) = Command::new("/usr/bin/security")
                                .arg("remove-trusted-cert")
                                .arg("-d")
                                .arg(&path)
                                .output()
                            {
                                let combined = format!(
                                    "{}{}",
                                    String::from_utf8_lossy(&out1.stdout),
                                    String::from_utf8_lossy(&out1.stderr)
                                );
                                if user_cancelled(&combined) {
                                    return Err("操作已取消：用户在授权对话框中点击了取消".into());
                                }
                            }
                            // Remove from user domain
                            if let Ok(out2) = Command::new("/usr/bin/security")
                                .arg("remove-trusted-cert")
                                .arg(&path)
                                .output()
                            {
                                let combined = format!(
                                    "{}{}",
                                    String::from_utf8_lossy(&out2.stdout),
                                    String::from_utf8_lossy(&out2.stderr)
                                );
                                if user_cancelled(&combined) {
                                    return Err("操作已取消：用户在授权对话框中点击了取消".into());
                                }
                            }
                        }
                        in_pem = false;
                    }
                }
            }
        }
    }

    // regexes
    let re_hash =
        Regex::new(r"SHA-(?:256|1) hash:\s*([0-9A-F: ]{40,191})").map_err(|e| e.to_string())?;
    // 1) Aggregate candidate keychains from user/system search lists
    let mut kc_set: HashSet<String> = HashSet::new();
    for domain in ["user", "system"].iter() {
        if let Ok(out) = Command::new("/usr/bin/security")
            .arg("list-keychains")
            .arg("-d")
            .arg(domain)
            .output()
        {
            let s = String::from_utf8_lossy(&out.stdout);
            for line in s.lines() {
                if let Some(start) = line.find('"') {
                    if let Some(end) = line[start + 1..].find('"') {
                        kc_set.insert(line[start + 1..start + 1 + end].to_string());
                    }
                }
            }
        }
    }
    // Add common paths
    if let Ok(home) = std::env::var("HOME") {
        kc_set.insert(format!("{}/Library/Keychains/login.keychain-db", home));
    }
    kc_set.insert("/Library/Keychains/System.keychain".to_string());
    kc_set.insert("/System/Library/Keychains/SystemRootCertificates.keychain".to_string());

    let mut any_found = false;
    let mut any_deleted = false;
    let mut last_err: Option<String> = None;
    for kc in kc_set.iter() {
        // Find certs in this keychain
        let out = Command::new("/usr/bin/security")
            .arg("find-certificate")
            .arg("-a")
            .arg("-Z")
            .arg("-c")
            .arg("PromptDumper Root CA")
            .arg(kc)
            .output();
        let out = match out {
            Ok(v) => v,
            Err(e) => {
                last_err = Some(e.to_string());
                continue;
            }
        };
        let s = String::from_utf8_lossy(&out.stdout);
        for line in s.lines() {
            if let Some(cap) = re_hash.captures(line) {
                any_found = true;
                let mut hash = cap[1].to_string();
                hash.retain(|c| c != ':' && c != ' ');
                // Try delete by hash
                let st_hash = Command::new("/usr/bin/security")
                    .arg("delete-certificate")
                    .arg("-Z")
                    .arg(&hash)
                    .arg(kc)
                    .output();
                match st_hash {
                    Ok(out) => {
                        if out.status.success() {
                            any_deleted = true;
                            continue;
                        }
                        let combined = format!(
                            "{}{}",
                            String::from_utf8_lossy(&out.stdout),
                            String::from_utf8_lossy(&out.stderr)
                        )
                        .to_lowercase();
                        if user_cancelled(&combined) {
                            return Err("操作已取消：用户在授权对话框中点击了取消".into());
                        }
                        if combined.contains("could not be found")
                            || combined.contains("unable to delete certificate")
                        {
                            any_deleted = true;
                            continue;
                        }
                        // Try by common name
                        let st_cn = Command::new("/usr/bin/security")
                            .arg("delete-certificate")
                            .arg("-c")
                            .arg("PromptDumper Root CA")
                            .arg(kc)
                            .output();
                        if let Ok(out_cn) = st_cn {
                            if out_cn.status.success() {
                                any_deleted = true;
                                continue;
                            }
                            let combined_cn = format!(
                                "{}{}",
                                String::from_utf8_lossy(&out_cn.stdout),
                                String::from_utf8_lossy(&out_cn.stderr)
                            )
                            .to_lowercase();
                            if user_cancelled(&combined_cn) {
                                return Err("操作已取消：用户在授权对话框中点击了取消".into());
                            }
                            if combined_cn.contains("could not be found")
                                || combined_cn.contains("unable to delete certificate")
                            {
                                any_deleted = true;
                                continue;
                            }
                        }
                        // If system keychain, escalate via osascript
                        if kc.starts_with("/Library/Keychains/")
                            || kc.starts_with("/System/Library/Keychains/")
                        {
                            let sh_cmd = format!(
                                "/usr/bin/security delete-certificate -Z {} '{}'",
                                hash, kc
                            );
                            let script = format!(
                                "do shell script \"{}\" with administrator privileges",
                                sh_cmd.replace("\\", "\\\\").replace("\"", "\\\"")
                            );
                            if let Ok(out3) = Command::new("/usr/bin/osascript")
                                .arg("-e")
                                .arg(&script)
                                .output()
                            {
                                let combined3 = format!(
                                    "{}{}",
                                    String::from_utf8_lossy(&out3.stdout),
                                    String::from_utf8_lossy(&out3.stderr)
                                );
                                if user_cancelled(&combined3) {
                                    return Err("操作已取消：用户在授权对话框中点击了取消".into());
                                }
                                if out3.status.success() {
                                    any_deleted = true;
                                    continue;
                                }
                            }
                            let sh_cmd2 = format!(
                                "/usr/bin/security delete-certificate -c 'PromptDumper Root CA' '{}'",
                                kc
                            );
                            let script2 = format!(
                                "do shell script \"{}\" with administrator privileges",
                                sh_cmd2.replace("\\", "\\\\").replace("\"", "\\\"")
                            );
                            if let Ok(out4) = Command::new("/usr/bin/osascript")
                                .arg("-e")
                                .arg(&script2)
                                .output()
                            {
                                let combined4 = format!(
                                    "{}{}",
                                    String::from_utf8_lossy(&out4.stdout),
                                    String::from_utf8_lossy(&out4.stderr)
                                );
                                if user_cancelled(&combined4) {
                                    return Err("操作已取消：用户在授权对话框中点击了取消".into());
                                }
                                if out4.status.success() {
                                    any_deleted = true;
                                    continue;
                                }
                            }
                        }
                        last_err = Some(format!(
                            "security failed to delete from {} (hash {})",
                            kc, hash
                        ));
                    }
                    Err(e) => {
                        last_err = Some(e.to_string());
                    }
                }
            }
        }
    }
    if !any_found {
        return Ok(());
    }
    if any_deleted {
        return Ok(());
    }
    Err(last_err
        .unwrap_or_else(|| "无法删除证书。可能需要手动在 钥匙串访问 中删除或需要管理员权限".into()))
}

#[cfg(not(target_os = "macos"))]
pub fn uninstall_ca_from_system_trust() -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use x509_parser::prelude::*;

    #[test]
    fn leaf_certificates_are_short_lived() {
        let (ca_pem, ca_key) = ensure_ca_exists().expect("ca generation");
        let (leaf_der, _, _) =
            generate_leaf_cert_for_host("example.com", &ca_pem, &ca_key).expect("leaf cert");
        let (_rest, cert) = parse_x509_certificate(&leaf_der).expect("parse cert");
        let validity = &cert.tbs_certificate.validity;
        let not_before = validity.not_before.to_datetime();
        let not_after = validity.not_after.to_datetime();
        let lifetime = not_after - not_before;
        assert!(
            lifetime <= Duration::days(397),
            "lifetime too long: {lifetime:?}"
        );
        let now = OffsetDateTime::now_utc();
        assert!(
            now >= not_before - Duration::days(2) && now <= not_after + Duration::days(2),
            "certificate validity window out of expected bounds"
        );
    }
}
