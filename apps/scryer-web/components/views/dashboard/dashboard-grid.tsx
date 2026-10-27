import "react-grid-layout/css/styles.css";
import "./dashboard-grid.css";

import * as React from "react";
import { GripVertical, Plus, RotateCcw, X } from "lucide-react";
import {
  Responsive,
  noCompactor,
  useContainerWidth,
  verticalCompactor,
  type Layout,
  type LayoutItem,
  type ResponsiveLayouts,
} from "react-grid-layout";
import { useClient } from "urql";

import { ConfirmDialog } from "@/components/common/confirm-dialog";
import { DASHBOARD_PANEL_ICONS } from "@/components/views/dashboard/dashboard-panel-icons";
import { DASHBOARD_PANEL_RENDERERS } from "@/components/views/dashboard/dashboard-panel-registry";
import {
  DashboardGridFallback,
  type DashboardPanelData,
} from "@/components/views/dashboard-view";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { IconButton } from "@/components/ui/icon-button";
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetHeader,
  SheetTitle,
} from "@/components/ui/sheet";
import { useGlobalStatus } from "@/lib/context/global-status-context";
import { useTranslate } from "@/lib/context/translate-context";
import { useUiSettings } from "@/lib/context/ui-settings-context";
import {
  resetMyDashboardLayoutMutation,
  setMyDashboardLayoutMutation,
} from "@/lib/graphql/mutations";
import type { UiSettings } from "@/lib/types";
import { cn } from "@/lib/utils";
import {
  DASHBOARD_BREAKPOINTS,
  DASHBOARD_COLUMNS,
  DASHBOARD_GRID_BREAKPOINTS,
  DASHBOARD_MARGIN,
  DASHBOARD_PANELS,
  DASHBOARD_PANEL_IDS,
  DASHBOARD_ROW_HEIGHT,
  appendDashboardPanel,
  changedDashboardBreakpoints,
  dashboardBreakpointForWidth,
  dashboardLayoutInput,
  dashboardPanelDefaultSize,
  dashboardPanelMinSize,
  dashboardPanelTier,
  effectiveDashboardLayouts,
  isDashboardPanelId,
  placeDashboardPanel,
  removeDashboardPanel,
  sameDashboardItems,
  type DashboardBreakpoint,
  type DashboardLayoutItem,
  type DashboardLayouts,
  type DashboardPanelId,
} from "@/lib/utils/dashboard-layout";
import { dashboardLayoutErrorMessage } from "@/lib/utils/dashboard-layout-error-message";

/**
 * The arrangeable dashboard grid. This module (and react-grid-layout with its
 * stylesheet) is loaded lazily from the dashboard container, so no other route
 * pays for it.
 *
 * Outside edit mode the grid is inert: nothing drags or resizes and no handles
 * render. In edit mode changes collect in a draft that only Save persists, one
 * mutation per breakpoint that changed; Cancel drops the draft.
 */

const DRAG_HANDLE_CLASS = "scry-dashboard-drag-handle";
const NO_DRAG_CLASS = "scry-dashboard-no-drag";
/** react-grid-layout's id for the placeholder of an item dragged in from outside. */
const DROPPING_ITEM_ID = "__dropping-elem__";

const GRID_BREAKPOINTS: Record<DashboardBreakpoint, number> = { ...DASHBOARD_GRID_BREAKPOINTS };
const GRID_COLUMNS: Record<DashboardBreakpoint, number> = { ...DASHBOARD_COLUMNS };
const CONTAINER_PADDING: readonly [number, number] = [0, 0];

type DashboardGridProps = {
  data: DashboardPanelData;
  editing: boolean;
  onEditingChange: (editing: boolean) => void;
};

function toGridLayout(items: readonly DashboardLayoutItem[], breakpoint: DashboardBreakpoint): LayoutItem[] {
  return items.map((entry) => {
    const min = isDashboardPanelId(entry.panelId)
      ? dashboardPanelMinSize(entry.panelId, breakpoint)
      : { w: 1, h: 1 };
    return { i: entry.panelId, x: entry.x, y: entry.y, w: entry.w, h: entry.h, minW: min.w, minH: min.h };
  });
}

function fromGridLayout(layout: Layout): DashboardLayoutItem[] {
  return layout
    .filter((entry) => entry.i !== DROPPING_ITEM_ID && isDashboardPanelId(entry.i))
    .map((entry) => ({ panelId: entry.i, x: entry.x, y: entry.y, w: entry.w, h: entry.h }));
}

export function DashboardGrid({ data, editing, onEditingChange }: DashboardGridProps) {
  const t = useTranslate();
  const client = useClient();
  const setGlobalStatus = useGlobalStatus();
  const { uiSettings, uiSettingsLoaded, setUiSettings } = useUiSettings();
  const { width, containerRef, mounted } = useContainerWidth({ measureBeforeMount: true });

  const saved = React.useMemo(
    () => effectiveDashboardLayouts(uiSettings.dashboardLayouts),
    [uiSettings.dashboardLayouts],
  );
  const [draft, setDraft] = React.useState<DashboardLayouts | null>(null);
  const [trayOpen, setTrayOpen] = React.useState(false);
  const [dragPanelId, setDragPanelId] = React.useState<DashboardPanelId | null>(null);
  const [saving, setSaving] = React.useState(false);
  const [confirmReset, setConfirmReset] = React.useState(false);

  const layouts = editing && draft ? draft : saved;
  const breakpoint = dashboardBreakpointForWidth(width);
  const items = layouts[breakpoint];
  const placed = React.useMemo(() => new Set(items.map((entry) => entry.panelId)), [items]);

  const gridLayouts = React.useMemo<ResponsiveLayouts<DashboardBreakpoint>>(() => {
    const result: ResponsiveLayouts<DashboardBreakpoint> = {};
    for (const entry of DASHBOARD_BREAKPOINTS) {
      result[entry] = toGridLayout(layouts[entry], entry);
    }
    return result;
  }, [layouts]);

  const updateDraft = React.useCallback(
    (change: (current: DashboardLayoutItem[]) => DashboardLayoutItem[]) => {
      setDraft((previous) => {
        const base = previous ?? saved;
        const next = change(base[breakpoint]);
        if (sameDashboardItems(next, base[breakpoint])) {
          return previous ?? base;
        }
        return { ...base, [breakpoint]: next };
      });
    },
    [breakpoint, saved],
  );

  const finish = React.useCallback(() => {
    setDraft(null);
    setTrayOpen(false);
    setDragPanelId(null);
    onEditingChange(false);
  }, [onEditingChange]);

  const handleLayoutChange = React.useCallback(
    (layout: Layout) => {
      if (!editing) {
        return;
      }
      const next = fromGridLayout(layout);
      updateDraft(() => next);
    },
    [editing, updateDraft],
  );

  const handleDrop = React.useCallback(
    (layout: Layout, item: LayoutItem | undefined) => {
      const panelId = dragPanelId;
      setDragPanelId(null);
      if (!editing || !panelId || !item) {
        return;
      }
      const rest = fromGridLayout(layout);
      updateDraft(() =>
        placeDashboardPanel(rest, panelId, breakpoint, { x: item.x, y: item.y, w: item.w, h: item.h }),
      );
    },
    [breakpoint, dragPanelId, editing, updateDraft],
  );

  const dropItemSize = React.useMemo(
    () => (dragPanelId ? dashboardPanelDefaultSize(dragPanelId, breakpoint) : { w: 1, h: 1 }),
    [breakpoint, dragPanelId],
  );

  const handleDropDragOver = React.useCallback(() => {
    if (!dragPanelId || placed.has(dragPanelId)) {
      return false;
    }
    return { w: dropItemSize.w, h: dropItemSize.h };
  }, [dragPanelId, dropItemSize, placed]);

  const reportFailure = React.useCallback(
    (error: unknown, fallbackKey: string) => {
      setGlobalStatus(dashboardLayoutErrorMessage(error, t, t(fallbackKey)), { level: "ERROR" });
    },
    [setGlobalStatus, t],
  );

  const save = React.useCallback(async () => {
    if (!draft) {
      finish();
      return;
    }
    const changed = changedDashboardBreakpoints(draft, saved);
    if (changed.length === 0) {
      finish();
      return;
    }
    setSaving(true);
    try {
      for (const entry of changed) {
        const result = await client
          .mutation<{ setMyDashboardLayout?: UiSettings }>(setMyDashboardLayoutMutation, {
            input: dashboardLayoutInput(entry, draft[entry]),
          })
          .toPromise();
        if (result.error || !result.data?.setMyDashboardLayout) {
          // Breakpoints saved before this one stay saved; the draft is kept so
          // nothing is lost and Save can be tried again.
          reportFailure(result.error, "dashboard.layout.saveFailed");
          return;
        }
        setUiSettings(result.data.setMyDashboardLayout);
      }
      setGlobalStatus(t("dashboard.layout.saved"), { level: "SUCCESS" });
      finish();
    } catch (error) {
      reportFailure(error, "dashboard.layout.saveFailed");
    } finally {
      setSaving(false);
    }
  }, [client, draft, finish, reportFailure, saved, setGlobalStatus, setUiSettings, t]);

  const reset = React.useCallback(async () => {
    setSaving(true);
    try {
      const result = await client
        .mutation<{ resetMyDashboardLayout?: UiSettings }>(resetMyDashboardLayoutMutation, {
          breakpoint: null,
        })
        .toPromise();
      if (result.error || !result.data?.resetMyDashboardLayout) {
        reportFailure(result.error, "dashboard.layout.resetFailed");
        return;
      }
      setUiSettings(result.data.resetMyDashboardLayout);
      setGlobalStatus(t("dashboard.layout.resetDone"), { level: "SUCCESS" });
      setConfirmReset(false);
      finish();
    } catch (error) {
      reportFailure(error, "dashboard.layout.resetFailed");
    } finally {
      setSaving(false);
    }
  }, [client, finish, reportFailure, setGlobalStatus, setUiSettings, t]);

  const stacked = breakpoint === "XS";

  return (
    <div ref={containerRef} className="flex min-w-0 flex-col gap-3">
      {editing ? (
        <div
          data-slot="dashboard-edit-toolbar"
          className="sticky top-0 z-20 flex min-w-0 flex-wrap items-center gap-2 rounded-xl border border-[var(--scry-accent)] bg-[var(--scry-bg)] px-3 py-2 shadow-sm"
        >
          <span className="flex min-w-0 flex-1 flex-col">
            <span className="text-[12px] font-semibold text-[var(--scry-ink2)]">
              {t("dashboard.layout.editing")}
            </span>
            <span className="text-[11px] text-[var(--scry-muted)]">
              {t(stacked ? "dashboard.layout.editHintStacked" : "dashboard.layout.editHint")}
            </span>
          </span>
          <Button type="button" size="sm" variant="secondary" onClick={() => setTrayOpen(true)}>
            <Plus className="h-3.5 w-3.5" aria-hidden="true" />
            {t("dashboard.layout.panels")}
          </Button>
          <Button
            type="button"
            size="sm"
            variant="secondary"
            disabled={saving}
            onClick={() => setConfirmReset(true)}
          >
            <RotateCcw className="h-3.5 w-3.5" aria-hidden="true" />
            {t("dashboard.layout.reset")}
          </Button>
          <Button type="button" size="sm" variant="secondary" disabled={saving} onClick={finish}>
            {t("label.cancel")}
          </Button>
          <Button type="button" size="sm" disabled={saving} onClick={() => void save()}>
            {t("label.save")}
          </Button>
        </div>
      ) : null}

      {!uiSettingsLoaded || !mounted ? (
        <DashboardGridFallback />
      ) : (
        <>
          {items.length === 0 ? (
            <p className="rounded-xl border border-dashed border-border px-3 py-6 text-center text-[12px] text-[var(--scry-muted2)]">
              {t("dashboard.layout.empty")}
            </p>
          ) : null}
          <Responsive
            className={cn("scry-dashboard-grid", editing && "scry-dashboard-grid-editing")}
            width={width}
            breakpoints={GRID_BREAKPOINTS}
            cols={GRID_COLUMNS}
            layouts={gridLayouts}
            rowHeight={DASHBOARD_ROW_HEIGHT}
            margin={DASHBOARD_MARGIN}
            containerPadding={CONTAINER_PADDING}
            compactor={stacked ? verticalCompactor : noCompactor}
            dragConfig={{
              enabled: editing,
              handle: `.${DRAG_HANDLE_CLASS}`,
              cancel: `.${NO_DRAG_CLASS}`,
              allowMobileScroll: true,
            }}
            resizeConfig={{ enabled: editing && !stacked, handles: ["s", "e", "se"] }}
            dropConfig={{
              enabled: editing,
              defaultItem: dropItemSize,
              onDragOver: handleDropDragOver,
              touchEnabled: true,
            }}
            onLayoutChange={handleLayoutChange}
            onDrop={handleDrop}
          >
            {items.map((entry) => {
              if (!isDashboardPanelId(entry.panelId)) {
                return null;
              }
              const panelId = entry.panelId;
              const tier = dashboardPanelTier(panelId, breakpoint, entry);
              const title = t(DASHBOARD_PANELS[panelId].titleKey);
              return (
                <div key={panelId} data-panel-id={panelId} data-tier={tier} className="scry-dashboard-item">
                  {DASHBOARD_PANEL_RENDERERS[panelId]({ data, tier, breakpoint })}
                  {editing ? (
                    <div
                      className={DRAG_HANDLE_CLASS}
                      role="group"
                      aria-label={t("dashboard.layout.movePanel", { name: title })}
                    >
                      <span className="scry-dashboard-drag-grip" aria-hidden="true">
                        <GripVertical className="h-4 w-4" />
                        <span className="truncate">{title}</span>
                      </span>
                      <IconButton
                        appearance="boxed"
                        tone="delete"
                        className={NO_DRAG_CLASS}
                        label={t("dashboard.layout.removePanel", { name: title })}
                        onClick={() => updateDraft((current) => removeDashboardPanel(current, panelId))}
                      >
                        <X className="h-3.5 w-3.5" aria-hidden="true" />
                      </IconButton>
                    </div>
                  ) : null}
                </div>
              );
            })}
          </Responsive>
        </>
      )}

      <DashboardPanelTray
        open={editing && trayOpen}
        onOpenChange={setTrayOpen}
        placed={placed}
        onDragPanel={setDragPanelId}
        onAddPanel={(panelId) => updateDraft((current) => appendDashboardPanel(current, panelId, breakpoint))}
      />

      <ConfirmDialog
        open={confirmReset}
        title={t("dashboard.layout.resetConfirmTitle")}
        description={t("dashboard.layout.resetConfirmDescription")}
        confirmLabel={t("dashboard.layout.reset")}
        cancelLabel={t("label.cancel")}
        isBusy={saving}
        onConfirm={() => void reset()}
        onCancel={() => setConfirmReset(false)}
      />
    </div>
  );
}

/**
 * Every panel there is, from the right. Non-modal so a panel can be dragged
 * out of it onto the grid while it stays open.
 */
function DashboardPanelTray({
  open,
  onOpenChange,
  placed,
  onDragPanel,
  onAddPanel,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  placed: ReadonlySet<string>;
  onDragPanel: (panelId: DashboardPanelId | null) => void;
  onAddPanel: (panelId: DashboardPanelId) => void;
}) {
  const t = useTranslate();

  return (
    <Sheet open={open} onOpenChange={onOpenChange} modal={false}>
      <SheetContent
        side="right"
        className="w-80 gap-0 sm:max-w-sm"
        onInteractOutside={(event) => event.preventDefault()}
      >
        <SheetHeader>
          <SheetTitle>{t("dashboard.layout.trayTitle")}</SheetTitle>
          <SheetDescription>{t("dashboard.layout.trayDescription")}</SheetDescription>
        </SheetHeader>
        <ul className="flex min-h-0 flex-1 flex-col gap-2 overflow-y-auto px-4 pb-4">
          {DASHBOARD_PANEL_IDS.map((panelId) => {
            const Icon = DASHBOARD_PANEL_ICONS[panelId];
            const spec = DASHBOARD_PANELS[panelId];
            const title = t(spec.titleKey);
            const isPlaced = placed.has(panelId);
            return (
              <li
                key={panelId}
                draggable={!isPlaced}
                onDragStart={(event) => {
                  event.dataTransfer.setData("text/plain", panelId);
                  event.dataTransfer.effectAllowed = "copy";
                  onDragPanel(panelId);
                }}
                onDragEnd={() => onDragPanel(null)}
                className={cn(
                  "flex min-w-0 items-start gap-2 rounded-lg border border-border px-2 py-2",
                  isPlaced ? "opacity-70" : "cursor-grab",
                )}
              >
                {isPlaced ? (
                  <span className="mt-0.5 h-4 w-4 shrink-0" aria-hidden="true" />
                ) : (
                  <span
                    data-rgl-draggable=""
                    className="scry-dashboard-tray-grip mt-0.5 shrink-0 text-[var(--scry-muted2)]"
                    aria-label={t("dashboard.layout.dragPanel", { name: title })}
                    role="img"
                    onTouchStart={() => onDragPanel(panelId)}
                  >
                    <GripVertical className="h-4 w-4" aria-hidden="true" />
                  </span>
                )}
                <Icon className="mt-0.5 h-4 w-4 shrink-0 text-[var(--scry-muted2)]" aria-hidden="true" />
                <span className="flex min-w-0 flex-1 flex-col gap-0.5">
                  <span className="flex min-w-0 items-center gap-1.5">
                    <span className="truncate text-[12px] font-semibold text-[var(--scry-ink2)]">{title}</span>
                    {isPlaced ? (
                      <Badge tone="positive" className="shrink-0 px-1 py-0 text-[10px]">
                        {t("dashboard.layout.placed")}
                      </Badge>
                    ) : null}
                  </span>
                  <span className="text-[11px] text-[var(--scry-muted)]">{t(spec.descriptionKey)}</span>
                </span>
                {isPlaced ? null : (
                  <IconButton
                    appearance="boxed"
                    tone="accent"
                    label={t("dashboard.layout.addPanel", { name: title })}
                    onClick={() => onAddPanel(panelId)}
                  >
                    <Plus className="h-3.5 w-3.5" aria-hidden="true" />
                  </IconButton>
                )}
              </li>
            );
          })}
        </ul>
      </SheetContent>
    </Sheet>
  );
}
