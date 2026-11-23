import React, { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { Check, ChevronDown, ChevronRight, Image as ImageIcon, Send } from 'lucide-react';
import type {
  A2uiComponentNode,
  A2uiSurfacePayload,
  A2uiUserAction,
  SnowflakeId,
} from '../types/api';

interface A2uiSurfaceRendererProps {
  surface: A2uiSurfacePayload;
  disabled?: boolean;
  onUserAction: (action: A2uiUserAction) => void;
  onOpenPhotoById?: (photoId: SnowflakeId) => void;
}

function normalizeKey(bindPath?: string | null, fallbackId?: string): string {
  if (!bindPath) return fallbackId || 'value';
  return bindPath.replace(/^\/+/, '').replace(/\//g, '.') || fallbackId || 'value';
}

const CHART_PALETTE = [
  'var(--kd-accent)',
  'var(--kd-ink)',
  'color-mix(in srgb, var(--kd-accent) 62%, var(--kd-ink))',
  'var(--kd-muted)',
  'color-mix(in srgb, var(--kd-accent) 42%, var(--kd-surface))',
];

const FORM_BOUND_COMPONENTS = new Set([
  'Checkbox',
  'CheckboxGroup',
  'Switch',
  'ChoiceChips',
  'Slider',
  'Stepper',
  'TextField',
]);

export const A2uiSurfaceRenderer: React.FC<A2uiSurfaceRendererProps> = ({
  surface,
  disabled,
  onUserAction,
  onOpenPhotoById,
}) => {
  const [model, setModel] = useState<Record<string, unknown>>(() => ({
    ...(surface.data_model || {}),
  }));
  const [submitted, setSubmitted] = useState<boolean>(Boolean(surface.submitted));
  const [submittedNote, setSubmittedNote] = useState<string | null>(
    surface.submitted_summary || null,
  );
  const [debouncePending, setDebouncePending] = useState(false);
  const [activeTabs, setActiveTabs] = useState<Record<string, number>>({});
  const [openCollapses, setOpenCollapses] = useState<Record<string, boolean>>({});

  const debounceTimerRef = useRef<number | null>(null);
  const latestModelRef = useRef(model);
  latestModelRef.current = model;

  useEffect(() => {
    setSubmitted(Boolean(surface.submitted));
    setSubmittedNote(surface.submitted_summary || null);
  }, [surface.submitted, surface.submitted_summary]);

  useEffect(() => {
    return () => {
      if (debounceTimerRef.current) window.clearTimeout(debounceTimerRef.current);
    };
  }, []);

  const nodeMap = useMemo(() => {
    const m = new Map<string, A2uiComponentNode>();
    for (const c of surface.components) m.set(c.id, c);
    return m;
  }, [surface.components]);

  const hasFormInputs = useMemo(
    () => surface.components.some((c) => FORM_BOUND_COMPONENTS.has(c.component)),
    [surface.components],
  );
  const hasExplicitActionButtons = useMemo(
    () =>
      surface.components.some(
        (c) => c.component === 'Button' || c.component === 'ActionGroup',
      ),
    [surface.components],
  );

  // Deduplicate root Card title if it repeats `surface.title`
  const rootNode = nodeMap.get(surface.root_id);
  const rootCardTitle =
    rootNode?.component === 'Card' && typeof rootNode.props?.title === 'string'
      ? rootNode.props.title.trim()
      : '';
  const effectiveHeaderTitle = (surface.title || '').trim() || rootCardTitle;

  const isLocked = Boolean(disabled || submitted);

  const summarizeModel = useCallback(
    (dm: Record<string, unknown>, explicitLabel?: string): string => {
      if (explicitLabel) return explicitLabel;
      const parts: string[] = [];
      for (const [k, v] of Object.entries(dm)) {
        if (Array.isArray(v) && v.length > 0) {
          parts.push(v.join('、'));
        } else if (typeof v === 'boolean' && v) {
          parts.push(k);
        } else if (typeof v === 'string' && v.trim()) {
          parts.push(v.trim());
        } else if (typeof v === 'number') {
          parts.push(`${k}:${v}`);
        }
      }
      return parts.length > 0 ? `已选：${parts.join(' · ')}` : '已确认反馈';
    },
    [],
  );

  const fireAction = useCallback(
    (
      actionName: string,
      nextModel: Record<string, unknown>,
      componentId?: string,
      val?: unknown,
      label?: string,
    ) => {
      if (submitted || disabled) return;
      if (debounceTimerRef.current) {
        window.clearTimeout(debounceTimerRef.current);
        debounceTimerRef.current = null;
      }
      setDebouncePending(false);
      const summary = summarizeModel(nextModel, label);
      setSubmitted(true);
      setSubmittedNote(summary);
      onUserAction({
        surface_id: surface.surface_id,
        action_name: actionName,
        component_id: componentId || null,
        value: val,
        data_model: nextModel,
        summary_label: summary,
      });
    },
    [disabled, onUserAction, submitted, summarizeModel, surface.surface_id],
  );

  const updateBoundField = useCallback(
    (
      node: A2uiComponentNode,
      nextVal: unknown,
      opts?: { allOptionsCount?: number; humanLabel?: string; skipDebounce?: boolean },
    ) => {
      if (isLocked) return;
      const key = normalizeKey(node.bind_path, node.id);
      const nextModel = { ...latestModelRef.current, [key]: nextVal };
      latestModelRef.current = nextModel;
      setModel(nextModel);

      const trig = surface.trigger;
      if (trig.mode === 'immediate' && !opts?.skipDebounce) {
        fireAction(
          trig.action || 'ui_response',
          nextModel,
          node.id,
          nextVal,
          opts?.humanLabel,
        );
        return;
      }

      if (trig.mode === 'on_all_checked') {
        const targetKey = trig.path ? normalizeKey(trig.path) : key;
        const arr = nextModel[targetKey];
        if (
          Array.isArray(arr) &&
          opts?.allOptionsCount &&
          opts.allOptionsCount > 0 &&
          arr.length >= opts.allOptionsCount
        ) {
          fireAction(trig.action || 'all_checked', nextModel, node.id, arr);
          return;
        }
      }

      if (trig.mode === 'debounce' && !opts?.skipDebounce) {
        if (debounceTimerRef.current) window.clearTimeout(debounceTimerRef.current);
        setDebouncePending(true);
        const wait = Math.max(600, Math.min(4000, trig.delay_ms || 1400));
        debounceTimerRef.current = window.setTimeout(() => {
          fireAction(trig.action || 'ui_response', latestModelRef.current, node.id);
        }, wait);
      }
    },
    [fireAction, isLocked, surface.trigger],
  );

  const renderNode = (nodeId: string, depth = 0): React.ReactNode => {
    if (depth > 8) return null;
    const node = nodeMap.get(nodeId);
    if (!node) return null;
    const p = node.props || {};
    const key = normalizeKey(node.bind_path, node.id);
    const val = model[key];

    switch (node.component) {
      case 'Card': {
        const cardTitle = typeof p.title === 'string' ? p.title.trim() : '';
        // Flatten `Card` completely so LLM-generated `Card` roots NEVER create Russian-doll nested boxes!
        const showSubTitle =
          cardTitle &&
          cardTitle !== effectiveHeaderTitle &&
          !(effectiveHeaderTitle && effectiveHeaderTitle.includes(cardTitle));
        return (
          <div
            key={node.id}
            className={
              depth === 0
                ? 'space-y-3'
                : 'pt-2.5 border-t border-border/60 space-y-2.5'
            }
          >
            {showSubTitle && (
              <div className="text-[13px] font-medium text-ink">{cardTitle}</div>
            )}
            <div className="space-y-2.5">
              {(node.children || []).map((cid) => renderNode(cid, depth + 1))}
            </div>
          </div>
        );
      }

      case 'Column':
        return (
          <div key={node.id} className="flex flex-col gap-2">
            {(node.children || []).map((cid) => renderNode(cid, depth + 1))}
          </div>
        );

      case 'Row':
        return (
          <div key={node.id} className="flex flex-wrap items-stretch gap-2">
            {(node.children || []).map((cid) => (
              <div key={cid} className="flex-1 min-w-[110px]">
                {renderNode(cid, depth + 1)}
              </div>
            ))}
          </div>
        );

      case 'Tabs': {
        const labels = Array.isArray(p.labels)
          ? p.labels.map(String)
          : (node.children || []).map((_, i) => `选项 ${i + 1}`);
        const curIdx = activeTabs[node.id] ?? 0;
        const activeChild = (node.children || [])[curIdx];
        return (
          <div key={node.id} className="space-y-2">
            <div className="flex gap-1 p-1 rounded-input bg-overlay text-[11px]">
              {labels.map((lb, idx) => (
                <button
                  key={idx}
                  type="button"
                  onClick={() => setActiveTabs((s) => ({ ...s, [node.id]: idx }))}
                  className={`flex-1 py-1 px-2 rounded-slip transition-colors cursor-pointer ${
                    curIdx === idx ? 'bg-panel text-ink shadow-xs font-medium' : 'text-muted hover:text-ink'
                  }`}
                >
                  {lb}
                </button>
              ))}
            </div>
            {activeChild ? renderNode(activeChild, depth + 1) : null}
          </div>
        );
      }

      case 'Collapse': {
        const isOpen = openCollapses[node.id] ?? Boolean(p.default_open);
        return (
          <div key={node.id} className="rounded-slip border border-border bg-panel/60 overflow-hidden">
            <button
              type="button"
              onClick={() => setOpenCollapses((s) => ({ ...s, [node.id]: !isOpen }))}
              className="w-full px-3 py-2 flex items-center justify-between text-xs font-medium text-ink hover:bg-overlay/60 transition-colors cursor-pointer"
            >
              <span>{String(p.title || '展开详情')}</span>
              {isOpen ? <ChevronDown className="w-3.5 h-3.5 text-muted" /> : <ChevronRight className="w-3.5 h-3.5 text-muted" />}
            </button>
            {isOpen && (
              <div className="px-3 pb-2.5 pt-1 space-y-2 border-t border-border">
                {(node.children || []).map((cid) => renderNode(cid, depth + 1))}
              </div>
            )}
          </div>
        );
      }

      case 'Divider':
        return <hr key={node.id} className="border-border my-1" />;

      case 'Badge':
        return (
          <span
            key={node.id}
            className="inline-flex items-center px-2 py-0.5 rounded-chip bg-accent-soft text-accent text-[11px] font-medium"
          >
            {String(p.text || p.label || '')}
          </span>
        );

      case 'Text': {
        const variant = String(p.variant || 'body');
        const cls =
          variant === 'title'
            ? 'text-sm font-medium text-ink'
            : variant === 'caption'
              ? 'text-[11px] text-subtle'
              : 'text-xs text-muted leading-relaxed';
        return (
          <p key={node.id} className={cls}>
            {String(p.text || '')}
          </p>
        );
      }

      case 'PhotoItem': {
        const photoId = typeof p.photo_id === 'string' ? p.photo_id : undefined;
        const thumbUrl = typeof p.thumb_url === 'string' ? p.thumb_url : undefined;
        const caption = typeof p.caption === 'string' ? p.caption : '点击查看照片回忆';
        const date = typeof p.date === 'string' ? p.date : '';
        return (
          <button
            key={node.id}
            type="button"
            onClick={() => photoId && onOpenPhotoById?.(photoId)}
            className="w-full p-2 rounded-slip bg-paper-white border border-border hover:border-border-strong shadow-xs flex items-center gap-2.5 text-left transition-transform duration-150 hover:-translate-y-0.5 cursor-pointer"
          >
            {thumbUrl ? (
              <img
                src={thumbUrl}
                alt={caption}
                className="w-12 h-12 rounded-[4px] object-cover shrink-0 bg-overlay"
              />
            ) : (
              <div className="w-12 h-12 rounded-[4px] bg-overlay flex items-center justify-center text-muted shrink-0">
                <ImageIcon className="w-4 h-4" />
              </div>
            )}
            <div className="min-w-0 flex-1">
              <div className="text-xs font-medium text-ink truncate">{caption || '定格瞬间'}</div>
              {date && <div className="text-[10px] text-subtle tabular-nums mt-0.5">{date}</div>}
            </div>
          </button>
        );
      }

      case 'Metric':
        return (
          <div
            key={node.id}
            className="px-3.5 py-2.5 rounded-[12px] bg-overlay/55 flex flex-col justify-between"
          >
            <span className="text-[11px] text-muted">{String(p.label || '')}</span>
            <div className="flex items-baseline justify-between gap-1.5 mt-1">
              <span className="text-[17px] font-semibold text-ink tabular-nums tracking-tight">
                {String(p.value ?? '-')}
              </span>
              {p.delta !== undefined && (
                <span className="text-[11px] text-accent font-medium tabular-nums">
                  {String(p.delta)}
                </span>
              )}
            </div>
          </div>
        );

      case 'Progress': {
        const pct = Math.max(0, Math.min(100, Number(p.value ?? 0)));
        return (
          <div key={node.id} className="space-y-1">
            <div className="flex justify-between text-[11px]">
              <span className="text-ink">{String(p.label || '')}</span>
              <span className="text-muted tabular-nums">{pct}%</span>
            </div>
            <div className="h-1.5 rounded-full bg-overlay overflow-hidden">
              <div
                style={{ width: `${pct}%` }}
                className="h-full bg-accent rounded-full transition-transform duration-200"
              />
            </div>
          </div>
        );
      }

      case 'Chart':
        return (
          <A2uiChartPrimitive
            key={node.id}
            node={node}
            disabled={isLocked}
            onPointSelect={(ptLabel, ptVal) => {
              if (p.interactive) {
                fireAction(
                  String(p.action || 'chart_select'),
                  { ...latestModelRef.current, selected_point: ptLabel, selected_value: ptVal },
                  node.id,
                  ptLabel,
                  `查看图表项：${ptLabel}`,
                );
              }
            }}
          />
        );

      case 'Heatmap':
        return <A2uiHeatmapPrimitive key={node.id} node={node} />;

      case 'Table': {
        const cols = Array.isArray(p.columns) ? p.columns.map(String) : [];
        const rows = Array.isArray(p.rows) ? (p.rows as unknown[][]) : [];
        return (
          <div key={node.id} className="overflow-x-auto rounded-slip border border-border bg-panel">
            <table className="w-full text-left border-collapse text-[11px]">
              {cols.length > 0 && (
                <thead>
                  <tr className="border-b border-border bg-overlay/60 text-muted">
                    {cols.map((c, i) => (
                      <th key={i} className="py-1.5 px-2.5 font-medium">
                        {c}
                      </th>
                    ))}
                  </tr>
                </thead>
              )}
              <tbody>
                {rows.map((r, ri) => (
                  <tr key={ri} className="border-b last:border-b-0 border-border text-ink">
                    {(Array.isArray(r) ? r : []).map((cell, ci) => (
                      <td key={ci} className="py-1.5 px-2.5 tabular-nums">
                        {String(cell ?? '')}
                      </td>
                    ))}
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        );
      }

      case 'Timeline': {
        const items = Array.isArray(p.items)
          ? (p.items as Array<{ time?: string; title?: string; detail?: string }>)
          : [];
        return (
          <div key={node.id} className="pl-2 space-y-2 border-l border-border-strong ml-1">
            {items.map((it, idx) => (
              <div key={idx} className="relative pl-3 text-xs">
                <span className="absolute -left-[13px] top-1.5 w-2 h-2 rounded-full bg-accent" />
                {it.time && <div className="text-[10px] text-subtle tabular-nums">{it.time}</div>}
                <div className="font-medium text-ink">{it.title}</div>
                {it.detail && <div className="text-[11px] text-muted mt-0.5">{it.detail}</div>}
              </div>
            ))}
          </div>
        );
      }

      case 'Checkbox': {
        const checked = Boolean(val ?? p.default_checked);
        const label = String(p.label || '');
        return (
          <button
            key={node.id}
            type="button"
            disabled={isLocked}
            onClick={() =>
              updateBoundField(node, !checked, {
                humanLabel: `${!checked ? '勾选' : '取消'}：${label}`,
              })
            }
            className="w-full py-2 px-2.5 rounded-slip bg-panel border border-border hover:border-border-strong flex items-center gap-2.5 text-left text-xs text-ink transition-colors disabled:opacity-60 cursor-pointer"
          >
            <span
              className={`w-4 h-4 rounded-[4px] border flex items-center justify-center shrink-0 transition-colors ${
                checked ? 'bg-accent border-accent text-white' : 'border-border-strong bg-surface'
              }`}
            >
              {checked && <Check className="w-3 h-3 stroke-[2.5]" />}
            </span>
            <span className="flex-1">{label}</span>
          </button>
        );
      }

      case 'CheckboxGroup': {
        const rawOpts = Array.isArray(p.options) ? p.options : [];
        const opts = rawOpts.map((o) =>
          typeof o === 'string'
            ? { value: o, label: o }
            : {
                value: String((o as Record<string, unknown>).value ?? (o as Record<string, unknown>).label ?? ''),
                label: String((o as Record<string, unknown>).label ?? (o as Record<string, unknown>).value ?? ''),
              },
        );
        const selectedArr: string[] = Array.isArray(val) ? val.map(String) : [];
        return (
          <div key={node.id} className="space-y-1.5">
            {typeof p.label === 'string' && p.label && (
              <div className="text-[11px] font-medium text-muted">{p.label}</div>
            )}
            {opts.map((opt) => {
              const isChecked = selectedArr.includes(opt.value);
              return (
                <button
                  key={opt.value}
                  type="button"
                  disabled={isLocked}
                  onClick={() => {
                    const next = isChecked
                      ? selectedArr.filter((x) => x !== opt.value)
                      : [...selectedArr, opt.value];
                    updateBoundField(node, next, { allOptionsCount: opts.length });
                  }}
                  className={`w-full py-2 px-2.5 rounded-slip border flex items-center gap-2.5 text-left text-xs transition-colors disabled:opacity-60 cursor-pointer ${
                    isChecked
                      ? 'bg-accent-soft border-accent/40 text-ink font-medium'
                      : 'bg-panel border-border hover:border-border-strong text-ink'
                  }`}
                >
                  <span
                    className={`w-4 h-4 rounded-[4px] border flex items-center justify-center shrink-0 transition-colors ${
                      isChecked
                        ? 'bg-accent border-accent text-white'
                        : 'border-border-strong bg-surface'
                    }`}
                  >
                    {isChecked && <Check className="w-3 h-3 stroke-[2.5]" />}
                  </span>
                  <span className="flex-1">{opt.label}</span>
                </button>
              );
            })}
          </div>
        );
      }

      case 'Switch': {
        const on = Boolean(val ?? p.default_value);
        const label = String(p.label || '');
        return (
          <button
            key={node.id}
            type="button"
            disabled={isLocked}
            onClick={() =>
              updateBoundField(node, !on, {
                humanLabel: `${label}：${!on ? '是' : '否'}`,
              })
            }
            className="w-full py-2 px-2.5 rounded-slip bg-panel border border-border hover:border-border-strong flex items-center justify-between gap-2 text-xs text-ink transition-colors disabled:opacity-60 cursor-pointer"
          >
            <span>{label}</span>
            <span
              className={`w-8 h-4.5 rounded-full p-0.5 transition-colors flex items-center ${
                on ? 'bg-accent justify-end' : 'bg-overlay border border-border-strong justify-start'
              }`}
            >
              <span className="w-3.5 h-3.5 rounded-full bg-white shadow-xs" />
            </span>
          </button>
        );
      }

      case 'ChoiceChips': {
        const multiple = Boolean(p.multiple);
        const rawOpts = Array.isArray(p.options) ? p.options : [];
        const opts = rawOpts.map((o) =>
          typeof o === 'string'
            ? { value: o, label: o }
            : {
                value: String((o as Record<string, unknown>).value ?? (o as Record<string, unknown>).label ?? ''),
                label: String((o as Record<string, unknown>).label ?? (o as Record<string, unknown>).value ?? ''),
              },
        );
        return (
          <div key={node.id} className="space-y-1.5">
            {typeof p.label === 'string' && p.label && (
              <div className="text-[11px] text-muted">{p.label}</div>
            )}
            <div className="flex flex-wrap gap-1.5">
              {opts.map((opt) => {
                const active = multiple
                  ? Array.isArray(val) && val.map(String).includes(opt.value)
                  : String(val ?? '') === opt.value;
                return (
                  <button
                    key={opt.value}
                    type="button"
                    disabled={isLocked}
                    onClick={() => {
                      if (multiple) {
                        const cur = Array.isArray(val) ? val.map(String) : [];
                        const next = active
                          ? cur.filter((x) => x !== opt.value)
                          : [...cur, opt.value];
                        updateBoundField(node, next);
                      } else {
                        updateBoundField(node, opt.value, { humanLabel: opt.label });
                      }
                    }}
                    className={`px-2.5 py-1 rounded-chip text-xs border transition-colors disabled:opacity-60 cursor-pointer ${
                      active
                        ? 'bg-ink text-surface border-ink font-medium'
                        : 'bg-panel border-border text-ink hover:border-border-strong'
                    }`}
                  >
                    {opt.label}
                  </button>
                );
              })}
            </div>
          </div>
        );
      }

      case 'Slider':
      case 'Stepper': {
        const min = Number(p.min ?? 0);
        const max = Number(p.max ?? 100);
        const step = Number(p.step ?? 1);
        const numVal = Number(val ?? p.default_value ?? min);
        const unit = typeof p.unit === 'string' ? p.unit : '';
        return (
          <div key={node.id} className="p-2.5 rounded-slip bg-panel border border-border space-y-1.5">
            <div className="flex items-center justify-between text-xs">
              <span className="text-ink">{String(p.label || '调节')}</span>
              <span className="font-semibold text-accent tabular-nums">
                {numVal}
                {unit}
              </span>
            </div>
            {node.component === 'Slider' ? (
              <input
                type="range"
                min={min}
                max={max}
                step={step}
                disabled={isLocked}
                value={numVal}
                onChange={(e) => updateBoundField(node, Number(e.target.value))}
                className="w-full accent-[var(--kd-accent)] cursor-pointer"
              />
            ) : (
              <div className="flex items-center gap-2">
                <button
                  type="button"
                  disabled={isLocked || numVal <= min}
                  onClick={() => updateBoundField(node, Math.max(min, numVal - step))}
                  className="px-2.5 py-1 rounded-slip bg-overlay text-xs text-ink disabled:opacity-40 cursor-pointer"
                >
                  -
                </button>
                <div className="flex-1 text-center text-xs tabular-nums text-muted">
                  {min} ~ {max}
                </div>
                <button
                  type="button"
                  disabled={isLocked || numVal >= max}
                  onClick={() => updateBoundField(node, Math.min(max, numVal + step))}
                  className="px-2.5 py-1 rounded-slip bg-overlay text-xs text-ink disabled:opacity-40 cursor-pointer"
                >
                  +
                </button>
              </div>
            )}
          </div>
        );
      }

      case 'TextField':
        return (
          <input
            key={node.id}
            type="text"
            disabled={isLocked}
            value={String(val ?? '')}
            placeholder={String(p.placeholder || '补充说明…')}
            onChange={(e) => updateBoundField(node, e.target.value, { skipDebounce: true })}
            className="w-full h-8 px-2.5 text-xs rounded-input bg-panel border border-border text-ink placeholder:text-subtle focus:outline-none focus:border-border-strong disabled:opacity-60"
          />
        );

      case 'Button': {
        const label = String(p.label || '确定');
        const actionName = String(p.action || 'button_click');
        const variant = String(p.variant || 'primary');
        return (
          <button
            key={node.id}
            type="button"
            disabled={isLocked}
            onClick={() =>
              fireAction(
                actionName,
                { ...latestModelRef.current, [key]: p.value ?? label },
                node.id,
                p.value ?? label,
                label,
              )
            }
            className={`px-3 py-1.5 rounded-input text-xs font-medium transition-colors disabled:opacity-50 cursor-pointer ${
              variant === 'secondary'
                ? 'bg-overlay text-ink border border-border hover:border-border-strong'
                : 'bg-ink text-surface hover:opacity-90'
            }`}
          >
            {label}
          </button>
        );
      }

      case 'ActionGroup': {
        const rawOpts = Array.isArray(p.options) ? p.options : [];
        const opts = rawOpts.map((o) =>
          typeof o === 'string'
            ? { value: o, label: o }
            : {
                value: (o as Record<string, unknown>).value ?? (o as Record<string, unknown>).label,
                label: String((o as Record<string, unknown>).label ?? (o as Record<string, unknown>).value ?? ''),
              },
        );
        const actionName = String(p.action || 'choice');
        return (
          <div key={node.id} className="flex flex-wrap gap-1.5 pt-0.5">
            {opts.map((opt, idx) => (
              <button
                key={idx}
                type="button"
                disabled={isLocked}
                onClick={() =>
                  fireAction(
                    actionName,
                    { ...latestModelRef.current, [key]: opt.value },
                    node.id,
                    opt.value,
                    opt.label,
                  )
                }
                className={`px-3 py-1.5 rounded-chip text-xs font-medium transition-colors disabled:opacity-50 cursor-pointer ${
                  idx === 0
                    ? 'bg-ink text-surface'
                    : 'bg-panel border border-border hover:border-border-strong text-ink'
                }`}
              >
                {opt.label}
              </button>
            ))}
          </div>
        );
      }

      default:
        return null;
    }
  };

  return (
    <div className="w-full rounded-[16px] bg-surface/80 border border-border/80 p-4 space-y-3">
      {effectiveHeaderTitle && (
        <div className="flex items-center justify-between gap-2">
          <span className="text-[14px] font-semibold text-ink tracking-tight">
            {effectiveHeaderTitle}
          </span>
          {submitted && (
            <span className="px-2 py-0.5 rounded-chip bg-overlay text-[10px] text-muted flex items-center gap-1">
              <Check className="w-2.5 h-2.5 text-accent" />
              <span>已确认</span>
            </span>
          )}
        </div>
      )}

      {renderNode(surface.root_id)}

      {/* Debounce Auto-Send Bar */}
      {!submitted && debouncePending && (
        <div className="pt-1 flex items-center justify-between text-xs text-muted">
          <span>选好停顿后将自动同步…</span>
          <button
            type="button"
            onClick={() =>
              fireAction(
                surface.trigger.mode === 'debounce'
                  ? surface.trigger.action || 'ui_response'
                  : 'ui_response',
                latestModelRef.current,
              )
            }
            className="px-3 py-1 rounded-chip bg-ink text-surface text-[11px] font-medium flex items-center gap-1 cursor-pointer"
          >
            <Send className="w-2.5 h-2.5" />
            <span>立即确认</span>
          </button>
        </div>
      )}

      {/* Only show Submit button when the surface actually contains interactive form controls! */}
      {!submitted &&
        surface.trigger.mode === 'submit' &&
        hasFormInputs &&
        !hasExplicitActionButtons && (
          <div className="pt-1 flex justify-end">
            <button
              type="button"
              disabled={isLocked}
              onClick={() =>
                fireAction(
                  surface.trigger.action || 'submit_surface',
                  latestModelRef.current,
                  surface.root_id,
                )
              }
              className="px-3.5 py-1.5 rounded-chip bg-ink text-surface text-xs font-medium flex items-center gap-1.5 hover:opacity-90 disabled:opacity-45 cursor-pointer"
            >
              <Check className="w-3 h-3" />
              <span>{surface.trigger.submit_label || '确认选择'}</span>
            </button>
          </div>
        )}

      {submitted && submittedNote && (
        <div className="text-xs text-muted pt-0.5 truncate">{submittedNote}</div>
      )}
    </div>
  );
};

// ============================================================================
// Pure-SVG Editorial Data Visualization Primitives (`Chart` & `Heatmap`)
// ============================================================================

const A2uiChartPrimitive: React.FC<{
  node: A2uiComponentNode;
  disabled: boolean;
  onPointSelect: (label: string, value: number) => void;
}> = ({ node, disabled, onPointSelect }) => {
  const p = node.props || {};
  const kind = String(p.kind || 'bar').toLowerCase();
  const rawData = Array.isArray(p.data) ? p.data : [];
  const data = rawData.map((d, idx) => {
    const rec = (d || {}) as Record<string, unknown>;
    return {
      label: String(rec.label ?? rec.name ?? `#${idx + 1}`),
      value: Number(rec.value ?? rec.count ?? 0),
    };
  });

  if (data.length === 0) return null;
  const maxVal = Math.max(1, ...data.map((d) => d.value));

  if (kind === 'donut' || kind === 'pie') {
    const total = Math.max(1, data.reduce((acc, d) => acc + Math.max(0, d.value), 0));
    const r = 34;
    const c = 2 * Math.PI * r;
    let offset = 0;

    return (
      <div className="p-2.5 rounded-slip bg-panel border border-border flex items-center gap-4">
        <svg viewBox="0 0 96 96" className="w-20 h-20 shrink-0 -rotate-90">
          <circle cx="48" cy="48" r={r} fill="none" stroke="var(--kd-overlay)" strokeWidth="12" />
          {data.map((d, i) => {
            const frac = Math.max(0, d.value) / total;
            const dash = frac * c;
            const segOffset = offset;
            offset += dash;
            return (
              <circle
                key={i}
                cx="48"
                cy="48"
                r={r}
                fill="none"
                stroke={CHART_PALETTE[i % CHART_PALETTE.length]}
                strokeWidth="12"
                strokeDasharray={`${dash} ${c - dash}`}
                strokeDashoffset={-segOffset}
              />
            );
          })}
        </svg>
        <div className="flex-1 min-w-0 space-y-1">
          {data.map((d, i) => (
            <button
              key={i}
              type="button"
              disabled={disabled}
              onClick={() => onPointSelect(d.label, d.value)}
              className="w-full flex items-center justify-between text-[11px] text-ink hover:opacity-80 cursor-pointer"
            >
              <span className="flex items-center gap-1.5 truncate">
                <span
                  className="w-2 h-2 rounded-full shrink-0"
                  style={{ backgroundColor: CHART_PALETTE[i % CHART_PALETTE.length] }}
                />
                <span className="truncate">{d.label}</span>
              </span>
              <span className="text-muted tabular-nums ml-2">{d.value}</span>
            </button>
          ))}
        </div>
      </div>
    );
  }

  if (kind === 'line' || kind === 'area') {
    const w = 280;
    const h = 104;
    const padX = 14;
    const padY = 14;
    const pts = data.map((d, i) => {
      const x =
        data.length === 1
          ? w / 2
          : padX + (i / (data.length - 1)) * (w - padX * 2);
      const y = h - padY - (Math.max(0, d.value) / maxVal) * (h - padY * 2);
      return { x, y, ...d };
    });
    const lineD = pts.map((pt, i) => `${i === 0 ? 'M' : 'L'}${pt.x.toFixed(1)},${pt.y.toFixed(1)}`).join(' ');
    const areaD = `${lineD} L${pts[pts.length - 1].x.toFixed(1)},${h - padY} L${pts[0].x.toFixed(1)},${h - padY} Z`;

    return (
      <div className="p-2.5 rounded-slip bg-panel border border-border space-y-1.5">
        <svg viewBox={`0 0 ${w} ${h}`} className="w-full h-24 overflow-visible">
          {kind === 'area' && <path d={areaD} fill="var(--kd-accent-soft)" />}
          <path d={lineD} fill="none" stroke="var(--kd-accent)" strokeWidth="2" strokeLinecap="round" />
          {pts.map((pt, i) => (
            <g
              key={i}
              onClick={() => !disabled && onPointSelect(pt.label, pt.value)}
              className="cursor-pointer"
            >
              <circle cx={pt.x} cy={pt.y} r="3.2" fill="var(--kd-panel)" stroke="var(--kd-accent)" strokeWidth="2" />
            </g>
          ))}
        </svg>
        <div className="flex justify-between text-[10px] text-subtle tabular-nums px-1">
          <span>{data[0]?.label}</span>
          {data.length > 2 && <span>{data[Math.floor(data.length / 2)]?.label}</span>}
          {data.length > 1 && <span>{data[data.length - 1]?.label}</span>}
        </div>
      </div>
    );
  }

  if (kind === 'radar' && data.length >= 3) {
    const cx = 64;
    const cy = 64;
    const radius = 42;
    const poly = data
      .map((d, i) => {
        const angle = (Math.PI * 2 * i) / data.length - Math.PI / 2;
        const r = (Math.max(0, d.value) / maxVal) * radius;
        return `${(cx + Math.cos(angle) * r).toFixed(1)},${(cy + Math.sin(angle) * r).toFixed(1)}`;
      })
      .join(' ');
    return (
      <div className="p-2.5 rounded-slip bg-panel border border-border flex items-center gap-3">
        <svg viewBox="0 0 128 128" className="w-28 h-28 shrink-0">
          <circle cx={cx} cy={cy} r={radius} fill="none" stroke="var(--kd-border-strong)" strokeDasharray="2 2" />
          <circle cx={cx} cy={cy} r={radius * 0.5} fill="none" stroke="var(--kd-border)" />
          <polygon points={poly} fill="var(--kd-accent-soft)" stroke="var(--kd-accent)" strokeWidth="1.8" />
        </svg>
        <div className="flex-1 space-y-1 text-[11px]">
          {data.map((d, i) => (
            <div key={i} className="flex justify-between text-ink">
              <span className="text-muted truncate">{d.label}</span>
              <span className="tabular-nums font-medium">{d.value}</span>
            </div>
          ))}
        </div>
      </div>
    );
  }

  // Default: Horizontal Proportional Bar Chart
  return (
    <div className="p-2.5 rounded-slip bg-panel border border-border space-y-2">
      {data.map((d, i) => {
        const pct = Math.max(4, Math.round((Math.max(0, d.value) / maxVal) * 100));
        return (
          <button
            key={i}
            type="button"
            disabled={disabled}
            onClick={() => onPointSelect(d.label, d.value)}
            className="w-full text-left space-y-1 group cursor-pointer"
          >
            <div className="flex justify-between text-[11px]">
              <span className="text-ink truncate">{d.label}</span>
              <span className="text-muted tabular-nums">{d.value}</span>
            </div>
            <div className="h-2 rounded-full bg-overlay overflow-hidden">
              <div
                style={{ width: `${pct}%` }}
                className="h-full rounded-full bg-accent group-hover:opacity-85 transition-opacity"
              />
            </div>
          </button>
        );
      })}
    </div>
  );
};

const A2uiHeatmapPrimitive: React.FC<{ node: A2uiComponentNode }> = ({ node }) => {
  const p = node.props || {};
  const mapObj = (p.values || p.data || {}) as Record<string, number>;
  const entries = Object.entries(mapObj).slice(-28);
  const max = Math.max(1, ...entries.map(([, v]) => Number(v) || 0));

  if (entries.length === 0) return null;

  return (
    <div className="p-2.5 rounded-slip bg-panel border border-border space-y-1.5">
      {typeof p.label === 'string' && p.label && (
        <div className="text-[11px] text-muted">{p.label}</div>
      )}
      <div className="grid grid-cols-7 gap-1">
        {entries.map(([day, count]) => {
          const intensity = Math.min(1, Math.max(0.12, Number(count) / max));
          return (
            <div
              key={day}
              title={`${day}: ${count}`}
              style={{
                backgroundColor:
                  Number(count) > 0
                    ? `color-mix(in srgb, var(--kd-accent) ${Math.round(intensity * 88)}%, var(--kd-overlay))`
                    : 'var(--kd-overlay)',
              }}
              className="h-4 rounded-[3px] border border-border"
            />
          );
        })}
      </div>
    </div>
  );
};
