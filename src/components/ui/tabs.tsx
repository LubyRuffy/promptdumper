import * as TabsPrimitive from "@radix-ui/react-tabs";
import { clsx } from "clsx";
import React from "react";

export const Tabs = TabsPrimitive.Root;
export const TabsList = React.forwardRef<HTMLDivElement, React.ComponentPropsWithoutRef<typeof TabsPrimitive.List>>(
  ({ className, ...props }, ref) => (
    <TabsPrimitive.List ref={ref} className={clsx("inline-flex h-8 items-center justify-center rounded-md bg-muted p-1 text-muted-foreground", className)} {...props} />
  )
);
TabsList.displayName = TabsPrimitive.List.displayName;

export const TabsTrigger = React.forwardRef<HTMLButtonElement, React.ComponentPropsWithoutRef<typeof TabsPrimitive.Trigger>>(
  ({ className, ...props }, ref) => (
    <TabsPrimitive.Trigger ref={ref} className={clsx("inline-flex items-center justify-center whitespace-nowrap rounded-sm px-3 py-1 text-xs font-medium  focus-visible:outline-none disabled:pointer-events-none disabled:opacity-50 data-[state=active]:bg-background data-[state=active]:text-foreground data-[state=active]:shadow", className)} {...props} />
  )
);
TabsTrigger.displayName = TabsPrimitive.Trigger.displayName;

export const TabsContent = React.forwardRef<HTMLDivElement, React.ComponentPropsWithoutRef<typeof TabsPrimitive.Content>>(
  ({ className, ...props }, ref) => (
    <TabsPrimitive.Content ref={ref} className={clsx("mt-2 focus-visible:outline-none", className)} {...props} />
  )
);
TabsContent.displayName = TabsPrimitive.Content.displayName;


