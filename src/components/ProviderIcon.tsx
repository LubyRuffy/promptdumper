import * as Icons from '@lobehub/icons';
import React from 'react';

export type ProviderIconName =
  | 'openai'
  | 'openai_compatible'
  | 'ollama'
  | 'lmstudio'
  | 'anthropic'
  | 'google'
  | string;

const providerToIcon: Record<string, React.ComponentType<{ size?: number }>> = {
  openai: (Icons as any).OpenAI,
  openai_compatible: (Icons as any).OpenAI,
  ollama: (Icons as any).Ollama,
  lmstudio: (Icons as any).LmStudio,
  anthropic: (Icons as any).Anthropic,
  google: (Icons as any).VertexAI,
};

interface Props {
  provider?: string | null;
  size?: number;
  className?: string;
}

export const ProviderIcon: React.FC<Props> = ({ provider, size = 14, className }) => {
  if (!provider) return null;
  const key = String(provider).toLowerCase();
  const Icon = providerToIcon[key];
  if (!Icon) return <span className={className}>🧠</span>;
  return <Icon size={size} />;
};
