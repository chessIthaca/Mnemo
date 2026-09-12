// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import * as React from "react";
import * as DialogPrimitive from "@radix-ui/react-dialog";

/**
 * Thin shadcn-style wrappers around Radix Dialog. These preserve the
 * existing visual design (dark overlay, bg-bg-secondary panel, border) while
 * adding the accessibility primitives Radix provides for free: role="dialog",
 * aria-modal, a focus trap, Escape-to-close, and focus restoration on close.
 *
 * The app components (ConfigDialog, SafetyToggleDialog) compose these
 * primitives and keep their existing Tailwind styling.
 */

const Dialog = DialogPrimitive.Root;
const DialogTrigger = DialogPrimitive.Trigger;
const DialogClose = DialogPrimitive.Close;

/** Full-screen dim overlay behind the dialog panel. */
const DialogOverlay = React.forwardRef<
  React.ElementRef<typeof DialogPrimitive.Overlay>,
  React.ComponentPropsWithoutRef<typeof DialogPrimitive.Overlay>
>(({ className, ...props }, ref) => (
  <DialogPrimitive.Overlay
    ref={ref}
    className={
      "fixed inset-0 z-50 bg-black/60 " + (className ?? "")
    }
    {...props}
  />
));
DialogOverlay.displayName = "DialogOverlay";

/**
 * The centered dialog panel. Renders the overlay + a positioned content
 * container. Callers pass their panel Tailwind classes via `className` (e.g.
 * width, border, bg) — this wrapper supplies positioning + the overlay.
 */
const DialogContent = React.forwardRef<
  React.ElementRef<typeof DialogPrimitive.Content>,
  React.ComponentPropsWithoutRef<typeof DialogPrimitive.Content>
>(({ className, children, ...props }, ref) => (
  <DialogPrimitive.Portal>
    <DialogOverlay />
    <DialogPrimitive.Content
      ref={ref}
      className={
        "fixed left-1/2 top-1/2 z-50 -translate-x-1/2 -translate-y-1/2 " +
        (className ?? "")
      }
      {...props}
    >
      {children}
    </DialogPrimitive.Content>
  </DialogPrimitive.Portal>
));
DialogContent.displayName = "DialogContent";

/** Accessible dialog title (screen readers announce it). */
const DialogTitle = React.forwardRef<
  React.ElementRef<typeof DialogPrimitive.Title>,
  React.ComponentPropsWithoutRef<typeof DialogPrimitive.Title>
>(({ className, ...props }, ref) => (
  <DialogPrimitive.Title ref={ref} className={className} {...props} />
));
DialogTitle.displayName = "DialogTitle";

/** Accessible dialog description. */
const DialogDescription = React.forwardRef<
  React.ElementRef<typeof DialogPrimitive.Description>,
  React.ComponentPropsWithoutRef<typeof DialogPrimitive.Description>
>(({ className, ...props }, ref) => (
  <DialogPrimitive.Description ref={ref} className={className} {...props} />
));
DialogDescription.displayName = "DialogDescription";

export {
  Dialog,
  DialogTrigger,
  DialogClose,
  DialogOverlay,
  DialogContent,
  DialogTitle,
  DialogDescription,
};
