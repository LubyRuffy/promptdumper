 
import { PanelGroup, Panel, PanelResizeHandle, type PanelGroupProps, type PanelProps } from "react-resizable-panels";
import { clsx } from "clsx";

export function ResizablePanelGroup({ className, ...props }: PanelGroupProps & { className?: string }) {
  return (
    <PanelGroup className={clsx("h-full w-full", className)} {...props} />
  );
}

export function ResizablePanel({ className, ...props }: PanelProps & { className?: string }) {
  return <Panel className={clsx("min-h-0", className)} {...props} />;
}

export function ResizableHandle({ className }: { className?: string }) {
  return (
    <PanelResizeHandle
      className={clsx(
        "shrink-0 bg-border transition-colors",
        "data-[panel-group-direction=vertical]:h-1 data-[panel-group-direction=vertical]:w-full",
        "data-[panel-group-direction=horizontal]:w-1 data-[panel-group-direction=horizontal]:h-full",
        "hover:bg-muted/50 focus-visible:outline-none",
        className,
      )}
    />
  );
}


