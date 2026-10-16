import type { ButtonHTMLAttributes, ReactNode } from "react";

/** The image action and a preview's Retry control are independent buttons. */
export function ImageAction({
  children,
  className,
  ...action
}: Omit<ButtonHTMLAttributes<HTMLButtonElement>, "children"> & {
  children: ReactNode;
}) {
  return (
    <div className={`image-action ${className || ""}`}>
      <button type="button" {...action} className="image-action-button" />
      {children}
    </div>
  );
}
