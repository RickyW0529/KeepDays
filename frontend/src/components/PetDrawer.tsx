import React, { useCallback, useEffect, useLayoutEffect, useRef, useState } from 'react';
import { AnimatePresence, motion } from 'framer-motion';
import {
  ArrowDown,
  Check,
  ChevronDown,
  Coffee,
  Compass,
  Heart,
  History,
  Loader2,
  Package,
  PawPrint,
  Plus,
  Send,
  ShoppingBag,
  Sparkles,
  Utensils,
  Wrench,
  X,
} from 'lucide-react';
import { api, streamAgentTurn } from '../lib/apiClient';
import { resolveMediaUrl } from '../lib/mediaProcessor';
import { A2uiSurfaceRenderer } from './A2uiSurfaceRenderer';
import { PetIllustrationSvg, type PetPose } from './RoamingPetLayer';
import type {
  A2uiSurfacePayload,
  A2uiUserAction,
  PetFullResp,
  SessionMetaDto,
  SessionTurnRecord,
  SnowflakeId,
  ToolTraceSummary,
} from '../types/api';

interface PetDrawerProps {
  spaceId: SnowflakeId;
  selfUserId: SnowflakeId;
  petResp: PetFullResp;
  onClose: () => void;
  onPetChanged: (next: PetFullResp) => void;
  onOpenPhotoById: (photoId: SnowflakeId) => void;
  onRefreshFridge?: () => void;
}

function renderInlineMarkdown(text: string): React.ReactNode[] {
  const parts = text.split(/(\*\*[^*]+\*\*|`[^`]+`)/g);
  return parts.map((part, idx) => {
    if (part.startsWith('**') && part.endsWith('**') && part.length > 4) {
      return (
        <strong key={idx} className="font-semibold text-ink">
          {part.slice(2, -2)}
        </strong>
      );
    }
    if (part.startsWith('`') && part.endsWith('`') && part.length > 2) {
      return (
        <code
          key={idx}
          className="px-1 py-0.5 rounded-[4px] bg-overlay font-mono text-[11px] text-ink"
        >
          {part.slice(1, -1)}
        </code>
      );
    }
    return <React.Fragment key={idx}>{part}</React.Fragment>;
  });
}

/** Clean streaming-friendly Markdown bubble body */
const ChatMarkdown: React.FC<{ text: string; streaming?: boolean }> = ({
  text,
  streaming = false,
}) => {
  const lines = text.replace(/\r\n/g, '\n').split('\n');
  const lastNonEmptyIdx = (() => {
    for (let i = lines.length - 1; i >= 0; i -= 1) {
      if (lines[i].trim().length > 0) return i;
    }
    return lines.length - 1;
  })();

  const cursor = streaming ? (
    <span className="inline-block w-[5px] h-3.5 ml-0.5 align-[-2px] rounded-[1px] bg-accent/85 animate-pulse" />
  ) : null;

  return (
    <div className="space-y-1 text-xs text-ink leading-relaxed break-words">
      {lines.map((raw, idx) => {
        const trimmed = raw.trim();
        const isTail = idx === lastNonEmptyIdx;
        if (!trimmed) {
          return <div key={idx} className="h-1" />;
        }
        if (/^#{1,3}\s+/.test(trimmed)) {
          return (
            <div key={idx} className="font-semibold text-ink pt-1">
              {renderInlineMarkdown(trimmed.replace(/^#{1,3}\s+/, ''))}
              {isTail && cursor}
            </div>
          );
        }
        const bulletMatch = trimmed.match(/^[-*•]\s+(.+)$/);
        if (bulletMatch) {
          return (
            <div key={idx} className="flex items-baseline gap-1.5 pl-0.5">
              <span className="w-1 h-1 rounded-full bg-accent/80 shrink-0 translate-y-[-3px]" />
              <span className="flex-1 min-w-0">
                {renderInlineMarkdown(bulletMatch[1])}
                {isTail && cursor}
              </span>
            </div>
          );
        }
        const numMatch = trimmed.match(/^(\d+[.)、])\s*(.+)$/);
        if (numMatch) {
          return (
            <div key={idx} className="flex items-baseline gap-1.5 pl-0.5">
              <span className="text-[11px] text-muted tabular-nums shrink-0">{numMatch[1]}</span>
              <span className="flex-1 min-w-0">
                {renderInlineMarkdown(numMatch[2])}
                {isTail && cursor}
              </span>
            </div>
          );
        }
        return (
          <p key={idx} className="whitespace-pre-wrap">
            {renderInlineMarkdown(raw)}
            {isTail && cursor}
          </p>
        );
      })}
    </div>
  );
};

/** Unified Assistant Bubble: merges collapsible tool trace header + A2UI surfaces + thinking/reply body into ONE cohesive bubble */
const AssistantReplyBubble: React.FC<{
  petName: string;
  text: string;
  traces: ToolTraceSummary[];
  surfaces: A2uiSurfacePayload[];
  streaming?: boolean;
  disabledSurfaces?: boolean;
  onUserAction: (act: A2uiUserAction) => void;
  onOpenPhotoById: (photoId: SnowflakeId) => void;
}> = ({
  petName,
  text,
  traces,
  surfaces,
  streaming = false,
  disabledSurfaces = false,
  onUserAction,
  onOpenPhotoById,
}) => {
  const [expanded, setExpanded] = useState(false);

  const hasTraces = traces.length > 0;
  const hasSurfaces = surfaces.length > 0;
  const hasText = Boolean(text && text.trim());

  const running = streaming ? traces.find((t) => t.summary.startsWith('正在')) : undefined;
  const latest = running || traces[traces.length - 1];
  const canExpand = traces.length > 1;
  const waitingForFirstToken = streaming && !hasText && !hasSurfaces;

  if (!hasTraces && !hasSurfaces && !hasText && !streaming) {
    return null;
  }

  // Header label when tools exist: seamlessly absorbs "歪头思考中…" before text arrives so there are NEVER two separate boxes!
  const traceHeadline = hasTraces
    ? running
      ? running.summary
      : waitingForFirstToken
        ? `${traces.length === 1 ? latest.summary : `已联动 ${traces.length} 项感知`} · 歪头思考中…`
        : traces.length === 1
          ? latest.summary
          : `已联动 ${traces.length} 项感知 · ${latest.summary}`
    : '';

  const showSpinnerInHeader = Boolean(running || waitingForFirstToken);

  return (
    <div className="w-fit max-w-[90%] rounded-card rounded-bl-slip bg-surface border border-border text-ink overflow-hidden">
      {hasTraces && (
        <div className={hasText || hasSurfaces ? 'bg-overlay/35' : ''}>
          <button
            type="button"
            onClick={() => canExpand && setExpanded((v) => !v)}
            className={`w-full px-3.5 py-2 flex items-center justify-between gap-2 text-[11px] text-muted text-left transition-colors ${
              canExpand ? 'hover:text-ink cursor-pointer' : 'cursor-default'
            }`}
          >
            <span className="inline-flex items-center gap-1.5 min-w-0">
              {showSpinnerInHeader ? (
                <Loader2 className="w-3 h-3 text-accent animate-spin shrink-0" />
              ) : (
                <Wrench className="w-2.5 h-2.5 text-accent/85 shrink-0" />
              )}
              <span className="truncate">{traceHeadline}</span>
            </span>
            {canExpand && (
              <ChevronDown
                className={`w-3 h-3 text-subtle shrink-0 transition-transform ${
                  expanded ? 'rotate-180' : ''
                }`}
              />
            )}
          </button>

          {expanded && canExpand && (
            <div className="px-3.5 pb-2.5 pt-1.5 border-t border-border/55 space-y-1 bg-overlay/20">
              {traces.map((tr) => (
                <div
                  key={tr.call_id}
                  className="flex items-center gap-1.5 text-[10px] text-muted leading-snug"
                >
                  {tr.summary.startsWith('正在') ? (
                    <Loader2 className="w-2.5 h-2.5 text-accent animate-spin shrink-0" />
                  ) : tr.ok ? (
                    <Check className="w-2.5 h-2.5 text-emerald-500 shrink-0" />
                  ) : (
                    <X className="w-2.5 h-2.5 text-rose-500 shrink-0" />
                  )}
                  <span className="truncate">{tr.summary}</span>
                </div>
              ))}
            </div>
          )}
        </div>
      )}

      {hasSurfaces && (
        <div
          className={`p-2.5 space-y-2 ${hasTraces ? 'border-t border-border/65' : ''}`}
        >
          {surfaces.map((surf) => (
            <A2uiSurfaceRenderer
              key={surf.surface_id}
              surface={surf}
              disabled={disabledSurfaces}
              onUserAction={onUserAction}
              onOpenPhotoById={onOpenPhotoById}
            />
          ))}
        </div>
      )}

      {hasText ? (
        <div
          className={`px-3.5 py-2.5 ${
            hasTraces || hasSurfaces ? 'border-t border-border/65' : ''
          }`}
        >
          <ChatMarkdown text={text} streaming={streaming} />
        </div>
      ) : !hasTraces && streaming ? (
        <div
          className={`px-3.5 py-2.5 text-xs text-muted inline-flex items-center gap-1.5 ${
            hasSurfaces ? 'border-t border-border/65' : ''
          }`}
        >
          <Loader2 className="w-3 h-3 animate-spin text-accent shrink-0" />
          <span>歪头思考中…</span>
        </div>
      ) : !streaming && !hasSurfaces && hasTraces ? (
        <div className="px-3.5 py-2 border-t border-border/65 text-xs text-muted italic">
          （{petName} 在冰箱顶上冲你眨了眨眼～）
        </div>
      ) : null}
    </div>
  );
};

export const PetDrawer: React.FC<PetDrawerProps> = ({
  spaceId,
  selfUserId,
  petResp,
  onClose,
  onPetChanged,
  onOpenPhotoById,
  onRefreshFridge,
}) => {
  const { pet, shop_catalog: catalog, users, asset_urls: assetUrls } = petResp;

  const [adoptName, setAdoptName] = useState('团子');
  const [adoptSpecies, setAdoptSpecies] = useState<'BLACK_CAT' | 'SHIBA' | 'OTTER' | 'BUNNY'>(
    'BLACK_CAT',
  );
  const [tab, setTab] = useState<'whisper' | 'care' | 'shop' | 'postcards'>('whisper');

  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const [reactPose, setReactPose] = useState<PetPose | null>(null);
  const [affectionCue, setAffectionCue] = useState<number>(0);
  const reactTimerRef = useRef<number | null>(null);

  // Agent Chat & S3 Session State
  const [sessions, setSessions] = useState<SessionMetaDto[]>([]);
  const [activeSessionId, setActiveSessionId] = useState<string>('');
  const [sessionMenuOpen, setSessionMenuOpen] = useState(false);
  const [turns, setTurns] = useState<SessionTurnRecord[]>([]);
  const [loadingChat, setLoadingChat] = useState(false);
  const [streaming, setStreaming] = useState(false);
  const [draft, setDraft] = useState('');
  const [streamText, setStreamText] = useState('');
  const [streamTools, setStreamTools] = useState<ToolTraceSummary[]>([]);
  const [streamSurfaces, setStreamSurfaces] = useState<A2uiSurfacePayload[]>([]);
  const chatScrollRef = useRef<HTMLDivElement | null>(null);
  const stickToBottomRef = useRef<boolean>(true);
  const [showJumpBottom, setShowJumpBottom] = useState(false);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', onKey);
    return () => {
      window.removeEventListener('keydown', onKey);
      if (reactTimerRef.current) window.clearTimeout(reactTimerRef.current);
    };
  }, [onClose]);

  const triggerReaction = useCallback((nextPose: PetPose, duration = 2800) => {
    if (reactTimerRef.current) window.clearTimeout(reactTimerRef.current);
    setReactPose(nextPose);
    setAffectionCue((n) => n + 1);
    reactTimerRef.current = window.setTimeout(() => {
      setReactPose(null);
    }, duration);
  }, []);

  const handleChatScroll = useCallback(() => {
    const el = chatScrollRef.current;
    if (!el) return;
    const distFromBottom = el.scrollHeight - el.scrollTop - el.clientHeight;
    const nearBottom = distFromBottom < 56;
    stickToBottomRef.current = nearBottom;
    setShowJumpBottom(!nearBottom);
  }, []);

  const scrollToBottom = useCallback((force = false, smooth = false) => {
    if (force) {
      stickToBottomRef.current = true;
      setShowJumpBottom(false);
    }
    const el = chatScrollRef.current;
    if (!el) return;
    if (force || stickToBottomRef.current) {
      if (smooth) {
        el.scrollTo({ top: el.scrollHeight, behavior: 'smooth' });
      } else {
        el.scrollTop = el.scrollHeight;
      }
    }
  }, []);

  // Synchronous post-DOM-commit sticky auto-scroll during live token & surface streaming!
  useLayoutEffect(() => {
    if (tab !== 'whisper') return;
    const el = chatScrollRef.current;
    if (!el || !stickToBottomRef.current) return;
    el.scrollTop = el.scrollHeight;
  }, [tab, activeSessionId, turns, streamText, streamTools, streamSurfaces, streaming]);

  const bootstrapSessions = useCallback(async () => {
    if (!pet.adopted) return;
    setLoadingChat(true);
    try {
      const res = await api.listAgentSessions(spaceId);
      setActiveSessionId(res.active_session_id);
      setSessions(res.sessions);
      setTurns(res.turns);
      stickToBottomRef.current = true;
      setShowJumpBottom(false);
    } catch {
      // ignore initial error
    } finally {
      setLoadingChat(false);
    }
  }, [pet.adopted, spaceId]);

  useEffect(() => {
    bootstrapSessions();
  }, [bootstrapSessions]);

  const switchSession = async (sid: string) => {
    setSessionMenuOpen(false);
    if (sid === activeSessionId) return;
    setLoadingChat(true);
    try {
      const detail = await api.getAgentSessionDetail(spaceId, sid);
      stickToBottomRef.current = true;
      setShowJumpBottom(false);
      setActiveSessionId(sid);
      setTurns(detail.turns);
    } catch (err) {
      setNotice(err instanceof Error ? err.message : '加载会话失败');
    } finally {
      setLoadingChat(false);
    }
  };

  const handleNewSession = async () => {
    setSessionMenuOpen(false);
    if (streaming) return;
    try {
      const created = await api.createAgentSession(spaceId);
      setActiveSessionId(created.session_id);
      setSessions((prev) => [created, ...prev]);
      setTurns([]);
      triggerReaction('stretching', 1800);
    } catch (err) {
      setNotice(err instanceof Error ? err.message : '开启新私语失败');
    }
  };

  const sendTurn = useCallback(
    async (opts: { text?: string; uiAction?: A2uiUserAction }) => {
      if (streaming) return;
      const cleanText = (opts.text || '').trim();
      if (!cleanText && !opts.uiAction) return;

      const optimisticTurn: SessionTurnRecord = {
        turn_id: `local_${Date.now()}`,
        role: 'user',
        text: cleanText || opts.uiAction?.summary_label || '已提交交互选项',
        ui_action_summary: opts.uiAction?.summary_label || null,
        tool_traces: [],
        surfaces: [],
        at: Date.now(),
      };

      if (opts.uiAction) {
        const act = opts.uiAction;
        setTurns((prev) => [
          ...prev.map((t) => ({
            ...t,
            surfaces: t.surfaces.map((s) =>
              s.surface_id === act.surface_id
                ? {
                    ...s,
                    data_model: act.data_model,
                    submitted: true,
                    submitted_summary: act.summary_label || '已确认',
                  }
                : s,
            ),
          })),
          optimisticTurn,
        ]);
      } else {
        setDraft('');
        setTurns((prev) => [...prev, optimisticTurn]);
      }

      stickToBottomRef.current = true;
      setShowJumpBottom(false);
      setStreaming(true);
      setStreamText('');
      setStreamTools([]);
      setStreamSurfaces([]);
      setReactPose('peeking');

      let accText = '';
      let accTools: ToolTraceSummary[] = [];
      let accSurfaces: A2uiSurfacePayload[] = [];
      let resolvedSessionId = activeSessionId;

      try {
        await streamAgentTurn(
          spaceId,
          {
            session_id: activeSessionId || null,
            text: cleanText,
            ui_action: opts.uiAction,
          },
          {
            onSessionMeta: (meta) => {
              if (meta.session_id) {
                resolvedSessionId = meta.session_id;
                setActiveSessionId(meta.session_id);
              }
            },
            onToolStart: (ev) => {
              const item: ToolTraceSummary = {
                call_id: ev.call_id,
                tool_name: ev.tool_name,
                ok: true,
                summary: `正在调用 ${ev.tool_name}…`,
              };
              accTools = [...accTools, item];
              setStreamTools(accTools);
              scrollToBottom();
            },
            onToolEnd: (ev) => {
              accTools = accTools.map((t) =>
                t.call_id === ev.call_id
                  ? { ...t, ok: ev.ok, summary: ev.summary }
                  : t,
              );
              setStreamTools(accTools);
              if (ev.tool_name.includes('fridge') || ev.tool_name === 'use_tool') {
                onRefreshFridge?.();
              }
              scrollToBottom();
            },
            onSurface: (surf) => {
              accSurfaces = [...accSurfaces, surf];
              setStreamSurfaces(accSurfaces);
              scrollToBottom();
            },
            onTextDelta: (delta) => {
              accText += delta;
              setStreamText(accText);
              scrollToBottom();
            },
            onTurnDone: (done) => {
              const fallbackIfSilent =
                !accText.trim() && accSurfaces.length === 0
                  ? '我在呢～刚刚在冰箱顶上伸了个懒腰，你想聊点什么呀？'
                  : accText;
              const finalTurn: SessionTurnRecord = {
                turn_id: done.turn_id,
                role: 'assistant',
                text: fallbackIfSilent,
                tool_traces: accTools,
                surfaces: accSurfaces,
                at: Date.now(),
              };
              setTurns((prev) => [...prev, finalTurn]);
              if (accTools.some((t) => t.tool_name === 'use_tool' || t.tool_name.includes('pet'))) {
                void api
                  .getPet(spaceId)
                  .then(onPetChanged)
                  .catch(() => {});
              }
              setSessions((prev) => {
                const exists = prev.some((s) => s.session_id === resolvedSessionId);
                if (!exists) {
                  return [
                    {
                      session_id: resolvedSessionId,
                      title: done.title || '与伙伴的私语',
                      status: 'ACTIVE',
                      turn_count: turns.length + 2,
                      last_active_at: Date.now(),
                      created_at: Date.now(),
                    },
                    ...prev,
                  ];
                }
                return prev.map((s) =>
                  s.session_id === resolvedSessionId
                    ? { ...s, title: done.title || s.title, last_active_at: Date.now() }
                    : s,
                );
              });
              triggerReaction('grooming', 2400);
            },
            onError: (msg) => {
              setNotice(msg);
              setReactPose(null);
            },
          },
        );
      } catch (err) {
        setNotice(err instanceof Error ? err.message : '发送失败');
        setReactPose(null);
      } finally {
        setStreaming(false);
        setStreamText('');
        setStreamTools([]);
        setStreamSurfaces([]);
      }
    },
    [activeSessionId, onPetChanged, onRefreshFridge, scrollToBottom, spaceId, streaming, triggerReaction, turns.length],
  );

  const defaultDrawerPose: PetPose =
    pet.current.behavior === 'SLEEPING_FRIDGE'
      ? 'sleeping'
      : pet.current.bed_style === 'cardboard_box'
        ? 'in_box'
        : 'sitting';

  const runAction = async (fn: () => Promise<PetFullResp>, reaction?: PetPose) => {
    if (busy) return;
    setBusy(true);
    setNotice(null);
    if (reaction) triggerReaction(reaction);
    try {
      const updated = await fn();
      onPetChanged(updated);
      if (updated.sweet_resonance_triggered) {
        setNotice('双人 5 分钟内同频互动，触发双倍羁绊与 +10 爪印币');
      }
    } catch (err) {
      setNotice(err instanceof Error ? err.message : '操作失败');
    } finally {
      setBusy(false);
    }
  };

  const selfNickname = users[selfUserId]?.profile.nickname || '你';
  const currentSessionMeta = sessions.find((s) => s.session_id === activeSessionId);

  return (
    <div className="fixed inset-0 z-40 pointer-events-none flex items-end sm:items-stretch justify-end">
      {/* Mobile-only subtle backdrop; Desktop stays 100% backdrop-free so left 75% gallery & fridge stay interactive! */}
      <motion.div
        initial={{ opacity: 0 }}
        animate={{ opacity: 1 }}
        exit={{ opacity: 0 }}
        onClick={onClose}
        className="fixed inset-0 bg-black/35 backdrop-blur-xs sm:hidden pointer-events-auto"
      />

      <motion.aside
        initial={{ x: 28, opacity: 0 }}
        animate={{ x: 0, opacity: 1 }}
        exit={{ x: 28, opacity: 0 }}
        transition={{ duration: 0.2, ease: [0.22, 1, 0.36, 1] }}
        className="pointer-events-auto relative z-10 w-full sm:w-[400px] h-[82dvh] sm:h-[calc(100dvh-5rem)] sm:mt-16 sm:mr-4 sm:mb-4 bg-panel/96 backdrop-blur-md border border-border-strong rounded-t-card sm:rounded-card shadow-float flex flex-col overflow-hidden transform-gpu"
      >
        {/* Top Companion Strip */}
        <div className="px-4 py-3 border-b border-border flex items-center justify-between gap-2 shrink-0 bg-surface/65">
          <div className="flex items-center gap-2.5 min-w-0">
            <div
              onClick={() =>
                pet.adopted &&
                triggerReaction(reactPose === 'stretching' ? 'grooming' : 'stretching')
              }
              className="relative w-10 h-10 rounded-input bg-paper-oat border border-border flex items-end justify-center pb-1 shrink-0 cursor-pointer overflow-hidden"
              title="轻拍互动"
            >
              <div className="w-8 h-7 flex items-end justify-center">
                <PetIllustrationSvg
                  species={pet.species || 'BLACK_CAT'}
                  pose={reactPose || defaultDrawerPose}
                  redScarf={pet.current.equipped_accessory === 'red_scarf'}
                  strawHat={pet.current.equipped_accessory === 'straw_hat'}
                />
              </div>
              <AnimatePresence>
                {affectionCue > 0 && reactPose && (
                  <motion.span
                    key={affectionCue}
                    initial={{ opacity: 0, y: 2 }}
                    animate={{ opacity: [0, 1, 0], y: -8 }}
                    transition={{ duration: 0.9 }}
                    className="absolute top-0.5 right-0.5 text-accent pointer-events-none"
                  >
                    <Heart className="w-2.5 h-2.5 fill-current" />
                  </motion.span>
                )}
              </AnimatePresence>
            </div>

            <div className="min-w-0">
              <div className="flex items-center gap-1.5">
                <h3 className="font-editorial text-base text-ink truncate">
                  {pet.adopted ? `${pet.name} · Lv.${pet.level}` : '领养共享伙伴'}
                </h3>
                {pet.adopted && (
                  <span className="px-1.5 py-0.5 rounded-chip bg-overlay text-[10px] text-muted tabular-nums">
                    {pet.paw_coins} 币
                  </span>
                )}
              </div>
              <div className="text-[11px] text-muted truncate">
                {pet.adopted
                  ? `与 ${selfNickname} 的悄悄话 · 随时陪伴`
                  : '住在冰箱顶上的数字小家守护生命'}
              </div>
            </div>
          </div>

          <div className="flex items-center gap-1 shrink-0">
            <button
              type="button"
              onClick={onClose}
              aria-label="收起抽屉"
              className="w-7 h-7 rounded-chip hover:bg-overlay flex items-center justify-center text-muted hover:text-ink transition-colors cursor-pointer"
            >
              <X className="w-4 h-4" />
            </button>
          </div>
        </div>

        {/* Unadopted Onboarding */}
        {!pet.adopted ? (
          <div className="flex-1 overflow-y-auto p-5 space-y-4">
            <form
              onSubmit={(e) => {
                e.preventDefault();
                runAction(() => api.adoptPet(spaceId, adoptName, adoptSpecies));
              }}
              className="space-y-4"
            >
              <p className="text-xs text-muted leading-relaxed">
                选择一只住在冰箱顶上的小家伙，它会记住你们的饮食偏好与日常点滴。
              </p>
              <div className="grid grid-cols-4 gap-2">
                {(
                  [
                    ['BLACK_CAT', '黑猫'],
                    ['SHIBA', '小柴'],
                    ['OTTER', '水獭'],
                    ['BUNNY', '垂耳兔'],
                  ] as const
                ).map(([code, label]) => (
                  <button
                    key={code}
                    type="button"
                    onClick={() => setAdoptSpecies(code)}
                    className={`py-2.5 px-2 rounded-slip border text-xs font-medium transition-colors cursor-pointer ${
                      adoptSpecies === code
                        ? 'bg-ink text-surface border-ink'
                        : 'bg-surface border-border text-muted hover:text-ink'
                    }`}
                  >
                    {label}
                  </button>
                ))}
              </div>
              <div>
                <label className="block text-xs text-muted mb-1.5">名字</label>
                <input
                  type="text"
                  required
                  value={adoptName}
                  onChange={(e) => setAdoptName(e.target.value)}
                  className="w-full px-3 py-2 text-sm bg-surface border border-border rounded-input text-ink focus:outline-none focus:border-border-strong"
                />
              </div>
              <button
                type="submit"
                disabled={busy}
                className="w-full py-2.5 rounded-input bg-ink text-surface text-sm font-medium cursor-pointer"
              >
                领养入住
              </button>
            </form>
          </div>
        ) : (
          /* Main 4-Tab Body (`私语 | 照料 | 杂货铺 | 纪念册`) */
          <>
            <div className="px-4 pt-2.5 shrink-0">
              <div className="grid grid-cols-4 p-1 rounded-input bg-overlay text-xs font-medium">
                {(
                  [
                    ['whisper', '私语'],
                    ['care', '照料'],
                    ['shop', '杂货铺'],
                    ['postcards', `纪念 (${pet.souvenirs.length})`],
                  ] as const
                ).map(([key, label]) => (
                  <button
                    key={key}
                    type="button"
                    onClick={() => setTab(key)}
                    className={`py-1.5 rounded-slip transition-colors cursor-pointer ${
                      tab === key ? 'bg-panel text-ink shadow-xs' : 'text-muted hover:text-ink'
                    }`}
                  >
                    {label}
                  </button>
                ))}
              </div>
            </div>

            {notice && (
              <div className="mx-4 mt-2 p-2 rounded-slip bg-accent-soft border border-accent/30 text-xs text-ink flex items-center justify-between gap-2 shrink-0">
                <span className="truncate">{notice}</span>
                <button
                  type="button"
                  onClick={() => setNotice(null)}
                  className="text-muted hover:text-ink"
                >
                  <X className="w-3 h-3" />
                </button>
              </div>
            )}

            {tab === 'whisper' ? (
              <div className="flex-1 min-h-0 flex flex-col">
                {/* Per-Speaker S3 Session Bar */}
                <div className="px-4 py-2 flex items-center justify-between gap-2 relative shrink-0">
                  <button
                    type="button"
                    onClick={() => setSessionMenuOpen((o) => !o)}
                    className="flex items-center gap-1.5 px-2.5 py-1 rounded-chip bg-surface border border-border hover:border-border-strong text-[11px] text-ink max-w-[230px] cursor-pointer"
                  >
                    <History className="w-3 h-3 text-muted shrink-0" />
                    <span className="truncate">
                      {currentSessionMeta?.title || '当前私语'}
                    </span>
                    <ChevronDown className="w-3 h-3 text-muted shrink-0" />
                  </button>

                  <button
                    type="button"
                    disabled={streaming}
                    onClick={handleNewSession}
                    title="封存当前话题进短期记忆，开启一段新私语"
                    className="flex items-center gap-1 px-2.5 py-1 rounded-chip bg-surface border border-border hover:border-border-strong text-[11px] text-ink disabled:opacity-45 cursor-pointer"
                  >
                    <Plus className="w-3 h-3 text-accent" />
                    <span>新私语</span>
                  </button>

                  {sessionMenuOpen && (
                    <div className="absolute top-9 left-4 right-4 z-30 max-h-56 overflow-y-auto rounded-card bg-panel border border-border-strong shadow-float p-1.5 space-y-1">
                      {sessions.map((s) => (
                        <button
                          key={s.session_id}
                          type="button"
                          onClick={() => switchSession(s.session_id)}
                          className={`w-full px-2.5 py-1.5 rounded-slip text-left text-xs flex items-center justify-between gap-2 cursor-pointer ${
                            s.session_id === activeSessionId
                              ? 'bg-overlay text-ink font-medium'
                              : 'text-muted hover:bg-surface hover:text-ink'
                          }`}
                        >
                          <span className="truncate">{s.title}</span>
                          <span className="text-[10px] text-subtle shrink-0">
                            {s.status === 'SEALED' ? '已封板归档' : '进行中'}
                          </span>
                        </button>
                      ))}
                    </div>
                  )}
                </div>

                {/* Conversation & Google A2UI Stream */}
                <div className="flex-1 min-h-0 relative flex flex-col">
                  <div
                    ref={chatScrollRef}
                    onScroll={handleChatScroll}
                    className="flex-1 overflow-y-auto px-4 py-2.5 space-y-3.5 scroll-smooth"
                    style={{ scrollBehavior: 'auto' }}
                  >
                    {loadingChat ? (
                      <div className="py-12 flex items-center justify-center text-xs text-muted gap-2">
                        <Loader2 className="w-4 h-4 animate-spin" />
                        <span>正在唤醒私语记录…</span>
                      </div>
                    ) : turns.length === 0 && !streaming ? (
                      <div className="py-6 space-y-3">
                        <div className="p-3.5 rounded-card bg-paper-oat/70 border border-border space-y-1.5">
                          <div className="text-xs font-medium text-ink flex items-center gap-1.5">
                            <PawPrint className="w-3.5 h-3.5 text-accent" />
                            <span>{pet.name} 正蹲在旁边听你说</span>
                          </div>
                          <p className="text-xs text-muted leading-relaxed">
                            你可以让我帮你看冰箱、挑今晚菜单（我会直接给你生成可打勾的动态小卡片）、统计相册数据画图表，或者偷偷跟我商量给另一半的小惊喜。
                          </p>
                        </div>

                        <div className="flex flex-wrap gap-1.5">
                          {[
                            '今晚菜单吃什么？给我列个打勾单',
                            '画张数据图看看我们小家的近期互动',
                            '帮我看眼冰箱上还有什么待办没做',
                          ].map((q) => (
                            <button
                              key={q}
                              type="button"
                              onClick={() => sendTurn({ text: q })}
                              className="px-2.5 py-1.5 rounded-chip bg-surface border border-border hover:border-border-strong text-[11px] text-ink transition-colors cursor-pointer"
                            >
                              {q}
                            </button>
                          ))}
                        </div>
                      </div>
                    ) : (
                      turns.map((t) => {
                        const isUser = t.role === 'user';
                        return (
                          <div
                            key={t.turn_id}
                            className={`flex flex-col ${
                              isUser ? 'items-end' : 'items-start'
                            }`}
                          >
                            {isUser ? (
                              t.text && (
                                <div className="max-w-[86%] px-3.5 py-2 rounded-card rounded-br-slip bg-ink text-surface text-xs leading-relaxed whitespace-pre-wrap break-words">
                                  {t.text}
                                </div>
                              )
                            ) : (
                              <AssistantReplyBubble
                                petName={pet.name}
                                text={t.text || ''}
                                traces={t.tool_traces || []}
                                surfaces={t.surfaces || []}
                                disabledSurfaces={streaming}
                                onUserAction={(act) => sendTurn({ uiAction: act })}
                                onOpenPhotoById={onOpenPhotoById}
                              />
                            )}
                          </div>
                        );
                      })
                    )}

                    {/* Active Streaming Bubble (Single Unified Bubble!) */}
                    {streaming && (
                      <div className="flex flex-col items-start pb-1">
                        <AssistantReplyBubble
                          petName={pet.name}
                          text={streamText}
                          traces={streamTools}
                          surfaces={streamSurfaces}
                          streaming
                          disabledSurfaces={streaming}
                          onUserAction={(act) => sendTurn({ uiAction: act })}
                          onOpenPhotoById={onOpenPhotoById}
                        />
                      </div>
                    )}
                  </div>

                  {/* Floating "Jump to Latest" Pill when user scrolled up */}
                  <AnimatePresence>
                    {showJumpBottom && (
                      <motion.button
                        type="button"
                        initial={{ opacity: 0, y: 6 }}
                        animate={{ opacity: 1, y: 0 }}
                        exit={{ opacity: 0, y: 6 }}
                        transition={{ duration: 0.14 }}
                        onClick={() => scrollToBottom(true, true)}
                        className="absolute bottom-2.5 left-1/2 -translate-x-1/2 z-20 px-3 py-1 rounded-chip bg-panel/95 border border-border-strong shadow-float text-[11px] text-ink flex items-center gap-1.5 hover:bg-surface transition-colors cursor-pointer"
                      >
                        {streaming && (
                          <span className="w-1.5 h-1.5 rounded-full bg-accent animate-pulse" />
                        )}
                        <span>{streaming ? '跟随流式输出' : '回到底部'}</span>
                        <ArrowDown className="w-3 h-3 text-muted" />
                      </motion.button>
                    )}
                  </AnimatePresence>
                </div>

                {/* Composer */}
                <form
                  onSubmit={(e) => {
                    e.preventDefault();
                    sendTurn({ text: draft });
                  }}
                  className="p-3 border-t border-border bg-surface/65 pb-safe flex items-center gap-2 shrink-0"
                >
                  <input
                    type="text"
                    disabled={streaming}
                    value={draft}
                    onChange={(e) => setDraft(e.target.value)}
                    placeholder={`跟 ${pet.name} 说点悄悄话…`}
                    className="flex-1 h-9 px-3 text-xs rounded-input bg-panel border border-border text-ink placeholder:text-subtle focus:outline-none focus:border-border-strong disabled:opacity-50"
                  />
                  <button
                    type="submit"
                    disabled={streaming || !draft.trim()}
                    className="h-9 px-3.5 rounded-input bg-ink text-surface text-xs font-medium flex items-center gap-1.5 disabled:opacity-40 cursor-pointer shrink-0"
                  >
                    <Send className="w-3.5 h-3.5" />
                    <span>发送</span>
                  </button>
                </form>
              </div>
            ) : (
              /* Care / Shop / Postcards Scrollable Area */
              <div className="flex-1 overflow-y-auto p-4 pb-safe space-y-4">
                {tab === 'care' && (
                  <div className="space-y-4">
                    <div className="grid grid-cols-4 gap-2 text-center">
                      {[
                        ['饱食', `${pet.stats.satiety}%`],
                        ['精力', `${pet.stats.energy}%`],
                        ['心情', `${pet.stats.mood}%`],
                        ['羁绊', `${pet.stats.bond}`],
                      ].map(([k, v]) => (
                        <div key={k} className="p-2.5 rounded-slip bg-surface border border-border">
                          <div className="text-[10px] text-muted">{k}</div>
                          <div className="text-sm font-semibold text-ink mt-0.5 tabular-nums">
                            {v}
                          </div>
                        </div>
                      ))}
                    </div>

                    <div className="p-3.5 rounded-card bg-surface border border-border space-y-2 text-[11px]">
                      <div className="flex items-center justify-between text-muted">
                        <span>独立</span>
                        <div className="w-36 h-1.5 rounded-full bg-overlay overflow-hidden">
                          <div
                            style={{ width: `${(pet.traits.clingy_vs_independent + 100) / 2}%` }}
                            className="h-full bg-ink"
                          />
                        </div>
                        <span>粘人</span>
                      </div>
                      <div className="flex items-center justify-between text-muted">
                        <span>好动</span>
                        <div className="w-36 h-1.5 rounded-full bg-overlay overflow-hidden">
                          <div
                            style={{ width: `${(pet.traits.foodie_vs_athletic + 100) / 2}%` }}
                            className="h-full bg-accent"
                          />
                        </div>
                        <span>贪吃</span>
                      </div>
                    </div>

                    <div className="grid grid-cols-3 sm:grid-cols-5 gap-2">
                      <button
                        type="button"
                        disabled={busy}
                        onClick={() =>
                          runAction(() => api.interactPet(spaceId, 'STROKE'), 'grooming')
                        }
                        className="p-2.5 rounded-slip bg-surface border border-border hover:border-border-strong text-[11px] text-ink flex flex-col items-center gap-1 cursor-pointer disabled:opacity-45"
                      >
                        <Heart className="w-3.5 h-3.5 text-accent" />
                        <span>顺毛</span>
                      </button>
                      <button
                        type="button"
                        disabled={busy}
                        onClick={() =>
                          runAction(() => api.interactPet(spaceId, 'PLAY'), 'stretching')
                        }
                        className="p-2.5 rounded-slip bg-surface border border-border hover:border-border-strong text-[11px] text-ink flex flex-col items-center gap-1 cursor-pointer disabled:opacity-45"
                      >
                        <Sparkles className="w-3.5 h-3.5 text-ink" />
                        <span>逗玩</span>
                      </button>
                      <button
                        type="button"
                        disabled={busy}
                        onClick={() =>
                          runAction(
                            () => api.interactPet(spaceId, 'FEED', 'salmon_freeze_dried'),
                            'grooming',
                          )
                        }
                        className="p-2.5 rounded-slip bg-surface border border-border hover:border-border-strong text-[11px] text-ink flex flex-col items-center gap-1 cursor-pointer disabled:opacity-45 tabular-nums"
                      >
                        <Utensils className="w-3.5 h-3.5 text-ink" />
                        <span>冻干 ({pet.inventory['salmon_freeze_dried'] || 0})</span>
                      </button>
                      <button
                        type="button"
                        disabled={busy}
                        onClick={() =>
                          runAction(
                            () => api.interactPet(spaceId, 'FEED', 'warm_goat_milk'),
                            'grooming',
                          )
                        }
                        className="p-2.5 rounded-slip bg-surface border border-border hover:border-border-strong text-[11px] text-ink flex flex-col items-center gap-1 cursor-pointer disabled:opacity-45 tabular-nums"
                      >
                        <Coffee className="w-3.5 h-3.5 text-ink" />
                        <span>羊奶 ({pet.inventory['warm_goat_milk'] || 0})</span>
                      </button>
                      <button
                        type="button"
                        disabled={busy || pet.current.behavior === 'ADVENTURING'}
                        onClick={() => runAction(() => api.startPetAdventure(spaceId), 'walking')}
                        className="col-span-2 sm:col-span-1 p-2.5 rounded-slip bg-surface border border-border hover:border-border-strong text-[11px] text-ink flex flex-col items-center gap-1 cursor-pointer disabled:opacity-45"
                      >
                        <Compass className="w-3.5 h-3.5 text-ink" />
                        <span>{pet.current.behavior === 'ADVENTURING' ? '散步中' : '探险'}</span>
                      </button>
                    </div>

                    <div className="p-3.5 rounded-card bg-overlay/70 border border-border flex items-center justify-between gap-3">
                      <div className="text-xs">
                        <div className="font-medium text-ink">双人合喂双拼罐</div>
                        <div className="text-[11px] text-muted mt-0.5">
                          {pet.coop_quest
                            ? `${users[pet.coop_quest.step1_by]?.profile.nickname || '伙伴'} 已开盖，等待接力`
                            : `库存 ${pet.inventory['duo_feast_can'] || 0} 罐 · 一人开罐，另一人添勺`}
                        </div>
                      </div>
                      {pet.coop_quest ? (
                        <button
                          type="button"
                          disabled={busy || pet.coop_quest.step1_by === selfUserId}
                          onClick={() =>
                            runAction(() => api.interactPet(spaceId, 'FINISH_DUO_CAN'))
                          }
                          className="px-3 py-1.5 rounded-chip bg-accent text-white text-xs font-medium disabled:opacity-45 cursor-pointer shrink-0"
                        >
                          {pet.coop_quest.step1_by === selfUserId ? '等TA接力' : '接力添勺'}
                        </button>
                      ) : (
                        <button
                          type="button"
                          disabled={busy || (pet.inventory['duo_feast_can'] || 0) === 0}
                          onClick={() =>
                            runAction(() => api.interactPet(spaceId, 'OPEN_DUO_CAN'))
                          }
                          className="px-3 py-1.5 rounded-chip bg-ink text-surface text-xs font-medium disabled:opacity-40 cursor-pointer shrink-0"
                        >
                          开罐
                        </button>
                      )}
                    </div>

                    {pet.logs.length > 0 && (
                      <div className="space-y-1.5 pt-1">
                        <div className="text-xs text-muted">互动记录</div>
                        <div className="space-y-1.5">
                          {pet.logs.slice(0, 20).map((l, idx) => {
                            const isPetAction =
                              l.action === 'AGENT_WHISPER' ||
                              l.action.startsWith('AGENT_') ||
                              l.action.startsWith('PET_') ||
                              !l.uid ||
                              l.uid === '0';
                            const actorName = isPetAction
                              ? pet.name
                              : users[l.uid]?.profile.nickname || '伙伴';

                            return (
                              <div
                                key={idx}
                                className="text-xs py-2 px-3 rounded-slip bg-surface border border-border/60 flex items-start justify-between gap-2.5 leading-relaxed"
                              >
                                <span className="text-ink min-w-0 break-words">
                                  <span
                                    className={`font-medium ${
                                      isPetAction ? 'text-accent' : 'text-ink'
                                    }`}
                                  >
                                    {actorName}
                                  </span>
                                  {isPetAction ? ' ' : '：'}
                                  <span className="text-ink/90">{l.text}</span>
                                </span>
                                <span className="text-[10px] text-subtle shrink-0 tabular-nums pt-0.5">
                                  {new Date(l.at).toLocaleTimeString('zh-CN', {
                                    hour: '2-digit',
                                    minute: '2-digit',
                                  })}
                                </span>
                              </div>
                            );
                          })}
                        </div>
                      </div>
                    )}
                  </div>
                )}

                {tab === 'shop' && (
                  <div className="space-y-2.5">
                    <p className="text-[11px] text-muted">
                      日常传照片 (+15)、留言 (+5)、贴今日菜单 (+20) 自动获得爪印币。
                    </p>
                    {catalog.map((item) => {
                      const ownedDecor = pet.unlocked_decor.includes(item.item_id);
                      const isEquipped =
                        pet.current.bed_style === item.item_id ||
                        pet.current.equipped_accessory === item.item_id;
                      return (
                        <div
                          key={item.item_id}
                          className="p-3 rounded-card bg-surface border border-border flex items-center justify-between gap-3"
                        >
                          <div>
                            <div className="text-xs font-medium text-ink flex items-center gap-1.5">
                              <ShoppingBag className="w-3.5 h-3.5 text-accent" />
                              <span>{item.name}</span>
                            </div>
                            <p className="text-[11px] text-muted mt-0.5">{item.desc}</p>
                          </div>
                          {ownedDecor ? (
                            <button
                              type="button"
                              onClick={() =>
                                runAction(() =>
                                  api.equipPetDecor(
                                    spaceId,
                                    item.category === 'BED'
                                      ? isEquipped
                                        ? 'linen_pad'
                                        : item.item_id
                                      : undefined,
                                    item.category === 'ACCESSORY'
                                      ? isEquipped
                                        ? null
                                        : item.item_id
                                      : pet.current.equipped_accessory,
                                  ),
                                )
                              }
                              className={`px-3 py-1.5 rounded-chip text-xs cursor-pointer shrink-0 ${
                                isEquipped
                                  ? 'bg-accent-soft text-accent border border-accent/30'
                                  : 'bg-overlay text-ink'
                              }`}
                            >
                              {isEquipped ? '使用中 · 卸下' : '换上'}
                            </button>
                          ) : (
                            <button
                              type="button"
                              disabled={busy || pet.paw_coins < item.price_coins}
                              onClick={() =>
                                runAction(() => api.buyPetItem(spaceId, item.item_id, 1))
                              }
                              className="px-3 py-1.5 rounded-chip bg-ink text-surface text-xs font-medium disabled:opacity-40 cursor-pointer shrink-0 tabular-nums"
                            >
                              {item.price_coins} 币
                            </button>
                          )}
                        </div>
                      );
                    })}
                  </div>
                )}

                {tab === 'postcards' && (
                  <div className="space-y-3">
                    {pet.souvenirs.length === 0 ? (
                      <div className="py-10 text-center text-xs text-subtle space-y-2">
                        <Package className="w-5 h-5 mx-auto opacity-50" />
                        <p>出门散步归来后，带回的明信片与标本会留在这里。</p>
                      </div>
                    ) : (
                      pet.souvenirs.map((sv) => {
                        const img = resolveMediaUrl(sv.ref_asset_id, assetUrls, true);
                        return (
                          <div
                            key={sv.id}
                            onClick={() => sv.ref_photo_id && onOpenPhotoById(sv.ref_photo_id)}
                            className="p-3.5 rounded-card bg-paper-oat border border-border shadow-paper flex gap-3 cursor-pointer"
                          >
                            {img && (
                              <img
                                src={img}
                                alt={sv.title}
                                className="w-16 h-16 rounded-slip object-cover shrink-0"
                              />
                            )}
                            <div className="space-y-1">
                              <div className="text-xs font-medium text-ink">{sv.title}</div>
                              <p className="text-xs text-muted leading-relaxed">{sv.diary}</p>
                              <div className="text-[10px] text-subtle tabular-nums">
                                {new Date(sv.at).toLocaleDateString('zh-CN')}
                              </div>
                            </div>
                          </div>
                        );
                      })
                    )}
                  </div>
                )}
              </div>
            )}
          </>
        )}
      </motion.aside>
    </div>
  );
};
