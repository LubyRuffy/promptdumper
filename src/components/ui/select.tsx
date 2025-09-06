import * as React from "react";
import * as SelectPrimitive from "@radix-ui/react-select";
import { Check, ChevronDown } from "lucide-react";
import { clsx } from "clsx";

export const Select = SelectPrimitive.Root;
export const SelectGroup = SelectPrimitive.Group;
export const SelectValue = SelectPrimitive.Value;

export function SelectTrigger(
  { className, children, hideChevron, variant = "default", ...props }: React.ComponentPropsWithoutRef<typeof SelectPrimitive.Trigger> & { hideChevron?: boolean; variant?: "default" | "icon" }
) {
  return (
    <SelectPrimitive.Trigger
      className={clsx(
        variant === "icon"
          ? [
              // icon-only button
              "flex h-8 w-8 items-center justify-center rounded-md p-0 text-sm",
              // no border/background, no ring
              "border-0 bg-transparent focus:outline-none focus:ring-0",
            ]
          : [
              // size & layout
              "flex h-9 w-[180px] items-center justify-between whitespace-nowrap rounded-md px-3 text-sm",
              // colors (match shadcn v4 tokens)
              "border border-input bg-background",
              // focus ring (v4: ring uses CSS variables)
              "focus:outline-none focus:ring-2 focus:ring-ring",
              // avoid content overflow
              "[&>span]:line-clamp-1",
            ],
        className
      )}
      {...props}
    >
      {children}
      {!hideChevron ? (
        <SelectPrimitive.Icon asChild>
          <ChevronDown className="h-4 w-4 opacity-50" />
        </SelectPrimitive.Icon>
      ) : null}
    </SelectPrimitive.Trigger>
  );
}

export function SelectContent({ className, children, ...props }: React.ComponentPropsWithoutRef<typeof SelectPrimitive.Content>) {
  return (
    <SelectPrimitive.Portal>
      <SelectPrimitive.Content
        position="popper"
        side="bottom"
        align="start"
        sideOffset={4}
        className={clsx(
          // extremely high z-index to sit above any app layer
          "z-[2147483647]",
          // container
          "min-w-[8rem] max-h-96 overflow-hidden rounded-md border bg-popover text-popover-foreground shadow-md",
          // shadcn-like entrance animation
          "data-[state=open]:animate-in data-[state=closed]:animate-out",
          "data-[state=closed]:fade-out-0 data-[state=open]:fade-in-0",
          "data-[state=closed]:zoom-out-95 data-[state=open]:zoom-in-95",
          "data-[side=bottom]:slide-in-from-top-2 data-[side=top]:slide-in-from-bottom-2",
          className
        )}
        {...props}
      >
        <SelectPrimitive.Viewport className="p-1">{children}</SelectPrimitive.Viewport>
      </SelectPrimitive.Content>
    </SelectPrimitive.Portal>
  );
}

export const SelectItem = React.forwardRef<
  React.ElementRef<typeof SelectPrimitive.Item>,
  React.ComponentPropsWithoutRef<typeof SelectPrimitive.Item>
>(({ className, children, ...props }, ref) => (
  <SelectPrimitive.Item
    ref={ref}
    className={clsx(
      "relative flex w-full cursor-default select-none items-center rounded-sm py-1.5 pl-8 pr-2 text-sm outline-none",
      // hover/focus style closer to shadcn
      "focus:bg-accent focus:text-accent-foreground",
      "data-[state=checked]:bg-accent data-[state=checked]:text-accent-foreground",
      "data-[disabled]:pointer-events-none data-[disabled]:opacity-50",
      className
    )}
    {...props}
  >
    <span className="absolute left-2 flex h-3.5 w-3.5 items-center justify-center">
      <SelectPrimitive.ItemIndicator>
        <Check className="h-4 w-4" />
      </SelectPrimitive.ItemIndicator>
    </span>
    <SelectPrimitive.ItemText>{children}</SelectPrimitive.ItemText>
  </SelectPrimitive.Item>
));
SelectItem.displayName = "SelectItem";


