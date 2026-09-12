// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

import * as React from "react";
import * as TabsPrimitive from "@radix-ui/react-tabs";

/**
 * Thin shadcn-style wrappers around Radix Tabs. Radix provides the tablist /
 * tab ARIA roles, aria-selected state, and left/right/Home/End arrow-key
 * navigation for free. The app components (MainPanel agent tabs, RightPanel
 * view tabs) compose these and keep their existing Tailwind active/inactive
 * styling via className + data-[state=active] selectors or render props.
 */

const Tabs = TabsPrimitive.Root;

/** The row of tab triggers. */
const TabsList = React.forwardRef<
  React.ElementRef<typeof TabsPrimitive.List>,
  React.ComponentPropsWithoutRef<typeof TabsPrimitive.List>
>(({ className, ...props }, ref) => (
  <TabsPrimitive.List ref={ref} className={className} {...props} />
));
TabsList.displayName = "TabsList";

/** A single tab button. */
const TabsTrigger = React.forwardRef<
  React.ElementRef<typeof TabsPrimitive.Trigger>,
  React.ComponentPropsWithoutRef<typeof TabsPrimitive.Trigger>
>(({ className, ...props }, ref) => (
  <TabsPrimitive.Trigger ref={ref} className={className} {...props} />
));
TabsTrigger.displayName = "TabsTrigger";

/** The panel rendered for the active tab. */
const TabsContent = React.forwardRef<
  React.ElementRef<typeof TabsPrimitive.Content>,
  React.ComponentPropsWithoutRef<typeof TabsPrimitive.Content>
>(({ className, ...props }, ref) => (
  <TabsPrimitive.Content ref={ref} className={className} {...props} />
));
TabsContent.displayName = "TabsContent";

export { Tabs, TabsList, TabsTrigger, TabsContent };
