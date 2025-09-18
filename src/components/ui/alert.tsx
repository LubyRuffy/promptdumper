import * as React from "react";
import { clsx } from "clsx";

export type AlertProps = React.HTMLAttributes<HTMLDivElement> & {
  variant?: "default" | "destructive" | "success" | "warning";
};

export const Alert = React.forwardRef<HTMLDivElement, AlertProps>(
  ({ className, variant = "default", ...props }, ref) => {
    const variantClasses =
      variant === "destructive"
        ? "border-destructive/50 text-destructive bg-destructive/10"
        : variant === "success"
        ? "border-green-500/50 text-green-600 dark:text-green-400 bg-green-50 dark:bg-green-950/40"
        : variant === "warning"
        ? "border-yellow-500/50 text-yellow-700 dark:text-yellow-300 bg-yellow-50 dark:bg-yellow-950/40"
        : "border-border bg-background text-foreground";
    return (
      <div
        ref={ref}
        role="alert"
        className={clsx(
          "w-full rounded-md border p-3 shadow-sm",
          variantClasses,
          className
        )}
        {...props}
      />
    );
  }
);
Alert.displayName = "Alert";

export const AlertTitle = React.forwardRef<
  HTMLHeadingElement,
  React.HTMLAttributes<HTMLHeadingElement>
>(({ className, ...props }, ref) => (
  <h5 ref={ref} className={clsx("mb-1 font-semibold leading-none tracking-tight", className)} {...props} />
));
AlertTitle.displayName = "AlertTitle";

export const AlertDescription = React.forwardRef<
  HTMLParagraphElement,
  React.HTMLAttributes<HTMLParagraphElement>
>(({ className, ...props }, ref) => (
  <div ref={ref} className={clsx("text-sm opacity-90", className)} {...props} />
));
AlertDescription.displayName = "AlertDescription";


