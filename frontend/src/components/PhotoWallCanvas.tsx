import React, { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { AnimatePresence, motion } from 'framer-motion';
import {
  AlertCircle,
  Film,
  Heart,
  ImagePlus,
  Loader2,
  MessageCircle,
  Plus,
  RotateCw,
  X,
} from 'lucide-react';
import { resolveMediaUrl } from '../lib/mediaProcessor';
import { useOverlay } from '../hooks/useOverlay';
import { toast } from './Toaster';
import type { AssetUrlMap, PhotoCardDto, SnowflakeId, UserMap } from '../types/api';

export interface QueuedPhotoUploadItem {
  file: File;
  preview: string;
  width: number;
  height: number;
}

interface PhotoWallCanvasProps {
  spaceId: SnowflakeId;
  photos: PhotoCardDto[];
  assetUrls: AssetUrlMap;
  usersMap: UserMap;
  viewingPhotos: Record<SnowflakeId, SnowflakeId[]>;
  compactDensity: boolean;
  hasMore: boolean;
  loadingMore: boolean;
  initialLoading: boolean;
  uploadModalOpen: boolean;
  onOpenUploadModal: () => void;
  onCloseUploadModal: () => void;
  onEnqueueUploads: (
    items: QueuedPhotoUploadItem[],
    meta: { caption: string; location: string; liveVideo: File | null },
  ) => void;
  onRetryUpload?: (tempPhotoId: SnowflakeId) => void;
  onDismissFailedUpload?: (tempPhotoId: SnowflakeId) => void;
  onSelectPhoto: (photo: PhotoCardDto) => void;
  onLoadMore: () => void;
}

interface PendingFile {
  file: File;
  preview: string;
  width: number;
  height: number;
}

interface MonthSection {
  key: string;
  year: string;
  month: string;
  items: PhotoCardDto[];
}

interface SpreadSlot {
  photo: PhotoCardDto;
  width: number;
  height: number;
  offsetTop: number;
  showSideNote?: boolean;
}

interface EditorialSpread {
  alignClass: string;
  gapPx: number;
  slots: SpreadSlot[];
}

const MAX_BATCH = 9;

function trueAspect(w: number, h: number): number {
  if (!w || !h) return 1;
  return Math.min(2.8, Math.max(0.42, w / h));
}

function fitUncroppedBox(
  aspect: number,
  maxW: number,
  maxH: number,
): { width: number; height: number } {
  const wByH = maxH * aspect;
  const width = Math.max(96, Math.round(Math.min(maxW, wByH)));
  const height = Math.max(96, Math.round(width / aspect));
  return { width, height };
}

/**
 * Compact Editorial Salon Composer:
 * Groups 2–4 mixed-aspect-ratio photos per row (avoiding lonely 1-photo orphans & giant holes)
 * while preserving subtle height hierarchy (89%–100%), gentle baseline stagger (6–18px),
 * and organic margin breathing room (88%–96% fill) — compact yet never rigid!
 */
function composeEditorialSpreads(
  items: PhotoCardDto[],
  fieldWidth: number,
  initialBeat = 0,
): { spreads: EditorialSpread[]; nextBeat: number } {
  if (items.length === 0 || fieldWidth <= 0) return { spreads: [], nextBeat: initialBeat };

  const isMobile = fieldWidth < 560;
  const isTablet = fieldWidth >= 560 && fieldWidth < 920;
  const spreads: EditorialSpread[] = [];
  let idx = 0;
  let beat = initialBeat;

  if (isMobile) {
    const gap = 10;
    while (idx < items.length) {
      const rem = items.length - idx;
      if (rem === 1) {
        const p = items[idx++];
        const asp = trueAspect(p.post.width, p.post.height);
        const box = fitUncroppedBox(asp, Math.round(fieldWidth * (asp < 0.9 ? 0.76 : 0.94)), 340);
        spreads.push({
          alignClass: beat % 2 === 0 ? 'justify-start' : 'justify-end',
          gapPx: gap,
          slots: [{ photo: p, ...box, offsetTop: 0 }],
        });
      } else {
        const p1 = items[idx++];
        const p2 = items[idx++];
        const a1 = trueAspect(p1.post.width, p1.post.height);
        const a2 = trueAspect(p2.post.width, p2.post.height);
        const mods = beat % 2 === 0 ? [1.0, 0.91] : [0.91, 1.0];
        const usableW = Math.round(fieldWidth * 0.97) - gap;
        const unitH = Math.min(250, usableW / (a1 * mods[0] + a2 * mods[1]));
        const h1 = Math.max(110, Math.round(unitH * mods[0]));
        const h2 = Math.max(110, Math.round(unitH * mods[1]));
        spreads.push({
          alignClass: 'justify-between items-start',
          gapPx: gap,
          slots: [
            { photo: p1, width: Math.round(h1 * a1), height: h1, offsetTop: beat % 2 === 0 ? 0 : 10 },
            { photo: p2, width: Math.round(h2 * a2), height: h2, offsetTop: beat % 2 === 0 ? 12 : 0 },
          ],
        });
      }
      beat += 1;
    }
    return { spreads, nextBeat: beat };
  }

  // Desktop & Tablet Compact Salon Cadence
  const gap = isTablet ? 16 : 20;
  const baseH = isTablet ? 275 : 335;
  const maxRowH = isTablet ? 330 : 395;
  const maxCols = isTablet ? 3 : 4;

  const SCALE_PATTERNS = [
    [1.0, 0.90, 0.96, 0.92],
    [0.92, 1.0, 0.91, 0.97],
    [0.97, 0.93, 1.0, 0.90],
  ];
  const OFFSET_PATTERNS = isTablet
    ? [
        [0, 12, 5, 10],
        [10, 0, 14, 4],
        [4, 14, 0, 10],
      ]
    : [
        [0, 18, 7, 14],
        [14, 0, 18, 6],
        [6, 16, 0, 12],
      ];
  const FILL_TARGETS = [0.93, 0.89, 0.95, 0.91];
  const ALIGN_CLASSES = [
    'justify-start items-start',
    'justify-end items-start',
    'justify-between items-start',
    'justify-start items-start pl-[2%]',
  ];

  while (idx < items.length) {
    const rem = items.length - idx;
    const scalePat = SCALE_PATTERNS[beat % SCALE_PATTERNS.length];
    const offsetPat = OFFSET_PATTERNS[beat % OFFSET_PATTERNS.length];
    const targetFill = FILL_TARGETS[beat % FILL_TARGETS.length];

    // Pick how many photos belong on this spread (2..maxCols, avoiding a 1-photo orphan next!)
    let take = 1;
    if (rem === 1) {
      take = 1;
    } else if (rem <= 3) {
      // Check if all `rem` items (2 or 3) fit nicely together without becoming tiny
      const sumAsp = items
        .slice(idx, idx + rem)
        .reduce((s, p, i) => s + trueAspect(p.post.width, p.post.height) * scalePat[i], 0);
      const estH = (fieldWidth * 0.94 - gap * (rem - 1)) / sumAsp;
      take = rem === 3 && estH < baseH * 0.68 ? 2 : rem;
    } else {
      // Greedily accumulate up to maxCols until natural width reaches ~85% of fieldWidth
      let accW = 0;
      take = 0;
      while (take < maxCols && idx + take < items.length) {
        const asp = trueAspect(items[idx + take].post.width, items[idx + take].post.height);
        const nextW = accW + asp * baseH * scalePat[take] + (take > 0 ? gap : 0);
        if (take >= 2 && nextW > fieldWidth * 0.98) break;
        accW = nextW;
        take += 1;
        if (take >= 2 && accW >= fieldWidth * 0.80) break;
      }
      take = Math.max(2, take);

      // Anti-orphan guard: never leave 1 lonely photo stranded at the end of the month!
      const leftAfter = rem - take;
      if (leftAfter === 1) {
        if (take < maxCols) {
          take += 1;
        } else {
          take -= 1;
        }
      }
    }

    const group = items.slice(idx, idx + take);
    idx += take;

    if (group.length === 1) {
      const p = group[0];
      const asp = trueAspect(p.post.width, p.post.height);
      const box = fitUncroppedBox(asp, Math.round(fieldWidth * (asp >= 1.2 ? 0.62 : 0.42)), maxRowH);
      spreads.push({
        alignClass: beat % 2 === 0 ? 'justify-start items-start' : 'justify-end items-start pr-[3%]',
        gapPx: gap,
        slots: [{ photo: p, ...box, offsetTop: 0 }],
      });
      beat += 1;
      continue;
    }

    const aspects = group.map((p) => trueAspect(p.post.width, p.post.height));
    const weightedAspSum = aspects.reduce((s, a, i) => s + a * scalePat[i], 0);
    const usableW = Math.round(fieldWidth * targetFill) - gap * (group.length - 1);
    const unitH = Math.min(maxRowH, usableW / weightedAspSum);

    const slots: SpreadSlot[] = group.map((p, i) => {
      const h = Math.max(140, Math.round(unitH * scalePat[i]));
      const w = Math.max(100, Math.round(h * aspects[i]));
      return {
        photo: p,
        width: w,
        height: h,
        offsetTop: offsetPat[i] || 0,
      };
    });

    const totalRowW = slots.reduce((s, sl) => s + sl.width, 0) + gap * (slots.length - 1);
    const alignClass =
      totalRowW >= fieldWidth * 0.88
        ? ALIGN_CLASSES[beat % ALIGN_CLASSES.length]
        : beat % 2 === 0
          ? 'justify-start items-start'
          : 'justify-center items-start';

    spreads.push({
      alignClass,
      gapPx: gap,
      slots,
    });
    beat += 1;
  }

  return { spreads, nextBeat: beat };
}

export const PhotoWallCanvas: React.FC<PhotoWallCanvasProps> = ({
  photos,
  assetUrls,
  usersMap,
  viewingPhotos,
  compactDensity,
  hasMore,
  loadingMore,
  initialLoading,
  uploadModalOpen,
  onOpenUploadModal,
  onCloseUploadModal,
  onEnqueueUploads,
  onRetryUpload,
  onDismissFailedUpload,
  onSelectPhoto,
  onLoadMore,
}) => {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const sentinelRef = useRef<HTMLDivElement | null>(null);
  const pressTimerRef = useRef<number | null>(null);

  const [containerWidth, setContainerWidth] = useState<number>(() =>
    typeof window !== 'undefined' ? Math.max(320, window.innerWidth - 64) : 1200,
  );
  const [liveHoverId, setLiveHoverId] = useState<SnowflakeId | null>(null);

  // Upload batch state
  const [pending, setPending] = useState<PendingFile[]>([]);
  const [liveVideo, setLiveVideo] = useState<File | null>(null);
  const [caption, setCaption] = useState('');
  const [location, setLocation] = useState('');
  const [dragActive, setDragActive] = useState(false);

  useOverlay(uploadModalOpen, onCloseUploadModal);

  // Measure gallery inner width accurately across resize / rotation (ignore <1px subpixel jitter)
  useEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    const update = () => {
      const w = Math.round(el.clientWidth);
      if (w > 0) {
        setContainerWidth((prev) => (Math.abs(prev - w) >= 1 ? w : prev));
      }
    };
    update();
    const ro = new ResizeObserver(update);
    ro.observe(el);
    return () => ro.disconnect();
  }, [photos.length, initialLoading]);

  const addFiles = useCallback((files: FileList | File[]) => {
    const images = Array.from(files).filter((f) => f.type.startsWith('image/'));
    if (images.length === 0) return;
    setPending((prev) => {
      const room = MAX_BATCH - prev.length;
      if (images.length > room) toast(`单次最多选择 ${MAX_BATCH} 张`, 'info');
      const accepted = images.slice(0, Math.max(0, room)).map((file) => {
        const preview = URL.createObjectURL(file);
        const item: PendingFile = { file, preview, width: 1600, height: 1200 };
        const probe = new Image();
        probe.onload = () => {
          if (probe.naturalWidth > 0 && probe.naturalHeight > 0) {
            setPending((curr) =>
              curr.map((c) =>
                c.preview === preview
                  ? { ...c, width: probe.naturalWidth, height: probe.naturalHeight }
                  : c,
              ),
            );
          }
        };
        probe.src = preview;
        return item;
      });
      return [...prev, ...accepted];
    });
  }, []);

  const resetUpload = (revokePreviews = true) => {
    if (revokePreviews) {
      pending.forEach((p) => URL.revokeObjectURL(p.preview));
    }
    setPending([]);
    setLiveVideo(null);
    setCaption('');
    setLocation('');
  };

  // Drag & drop anywhere on canvas
  useEffect(() => {
    let depth = 0;
    const hasFiles = (e: DragEvent) => Array.from(e.dataTransfer?.types || []).includes('Files');
    const onEnter = (e: DragEvent) => {
      if (!hasFiles(e)) return;
      depth += 1;
      setDragActive(true);
    };
    const onLeave = () => {
      depth = Math.max(0, depth - 1);
      if (depth === 0) setDragActive(false);
    };
    const onOver = (e: DragEvent) => {
      if (hasFiles(e)) e.preventDefault();
    };
    const onDrop = (e: DragEvent) => {
      if (!hasFiles(e)) return;
      e.preventDefault();
      depth = 0;
      setDragActive(false);
      if (e.dataTransfer?.files?.length) {
        addFiles(e.dataTransfer.files);
        onOpenUploadModal();
      }
    };
    window.addEventListener('dragenter', onEnter);
    window.addEventListener('dragleave', onLeave);
    window.addEventListener('dragover', onOver);
    window.addEventListener('drop', onDrop);
    return () => {
      window.removeEventListener('dragenter', onEnter);
      window.removeEventListener('dragleave', onLeave);
      window.removeEventListener('dragover', onOver);
      window.removeEventListener('drop', onDrop);
    };
  }, [addFiles, onOpenUploadModal]);

  useEffect(() => {
    const el = sentinelRef.current;
    if (!el || !hasMore) return;
    const io = new IntersectionObserver(
      (entries) => {
        if (entries[0]?.isIntersecting && !loadingMore) onLoadMore();
      },
      { rootMargin: '600px 0px' },
    );
    io.observe(el);
    return () => io.disconnect();
  }, [hasMore, loadingMore, onLoadMore]);

  // WeChat-style 0ms instant close + optimistic wall insertion + async background transfer!
  const handleUploadSubmit = (e: React.FormEvent) => {
    e.preventDefault();
    if (pending.length === 0) return;
    const batchSnapshot = pending.map((p) => ({ ...p }));
    const metaSnapshot = {
      caption,
      location,
      liveVideo: pending.length === 1 ? liveVideo : null,
    };
    resetUpload(false);
    onCloseUploadModal();
    onEnqueueUploads(batchSnapshot, metaSnapshot);
  };

  // Group chronologically by Year + Month
  const monthSections = useMemo<MonthSection[]>(() => {
    const map = new Map<string, MonthSection>();
    for (const p of photos) {
      const d = new Date(p.taken_at);
      const year = String(d.getFullYear());
      const month = String(d.getMonth() + 1).padStart(2, '0');
      const key = `${year}-${month}`;
      let sec = map.get(key);
      if (!sec) {
        sec = { key, year, month, items: [] };
        map.set(key, sec);
      }
      sec.items.push(p);
    }
    return Array.from(map.values());
  }, [photos]);

  // Effective exhibition field width (accounts for slimmer sticky left index column on lg+ screens)
  const isLargeDesktop = containerWidth >= 980;
  const stickyColOffset = isLargeDesktop ? 132 : 0;
  const fieldWidth = Math.max(260, containerWidth - stickyColOffset);

  const renderEditorialPrint = (
    p: PhotoCardDto,
    boxStyle: React.CSSProperties,
    opts: { eager?: boolean; preferFullRes?: boolean; showPlateCaption?: boolean } = {},
  ) => {
    const imgUrl = opts.preferFullRes
      ? resolveMediaUrl(p.asset_id, assetUrls, false) || resolveMediaUrl(p.asset_id, assetUrls, true)
      : resolveMediaUrl(p.asset_id, assetUrls, true) || resolveMediaUrl(p.asset_id, assetUrls, false);
    const liveUrl = resolveMediaUrl(p.post.live_asset_id, assetUrls, false);
    const viewers = viewingPhotos[p.id] || [];
    const isPartnerViewing = viewers.length > 0;
    const partnerName = viewers[0] ? usersMap[viewers[0]]?.profile.nickname : null;

    const d = new Date(p.taken_at);
    const dayStr = `${String(d.getMonth() + 1).padStart(2, '0')}.${String(d.getDate()).padStart(2, '0')}`;
    const noteLabel = p.post.caption?.trim() || p.post.exif?.location?.trim() || '';

    return (
      <div className="flex flex-col max-w-full">
        <figure
          data-photo-id={p.id}
          role="button"
          tabIndex={0}
          aria-label={p.post.caption || '查看照片'}
          onClick={() => onSelectPhoto(p)}
          onKeyDown={(e) => {
            if (e.key === 'Enter') onSelectPhoto(p);
          }}
          onPointerEnter={(ev) => {
            if (ev.pointerType === 'mouse' && liveUrl) setLiveHoverId(p.id);
          }}
          onPointerLeave={() => {
            if (liveHoverId === p.id) setLiveHoverId(null);
            if (pressTimerRef.current) window.clearTimeout(pressTimerRef.current);
          }}
          onPointerDown={(ev) => {
            if (ev.pointerType !== 'mouse' && liveUrl) {
              pressTimerRef.current = window.setTimeout(() => setLiveHoverId(p.id), 260);
            }
          }}
          onPointerUp={() => {
            if (pressTimerRef.current) window.clearTimeout(pressTimerRef.current);
            setLiveHoverId(null);
          }}
          style={{
            ...boxStyle,
            boxShadow: '0 14px 36px rgba(18, 16, 14, 0.08), 0 1px 4px rgba(18, 16, 14, 0.04)',
          }}
          className={`group relative overflow-hidden rounded-[2px] bg-overlay ring-1 ring-border hover:ring-border-strong transition-shadow duration-200 cursor-pointer select-none outline-none focus-visible:ring-2 focus-visible:ring-accent shrink-0 ${
            isPartnerViewing ? 'animate-presence-rim z-10' : ''
          }`}
        >
          {imgUrl ? <FadeImg src={imgUrl} alt={p.post.caption || ''} eager={opts.eager} /> : null}

          {liveUrl && liveHoverId === p.id && (
            <video
              src={liveUrl}
              autoPlay
              muted
              playsInline
              loop
              className="absolute inset-0 w-full h-full object-cover"
            />
          )}

          {p.post.live_asset_id && (
            <span className="absolute top-2.5 right-2.5 px-1.5 py-0.5 rounded-chip bg-black/35 backdrop-blur-md text-[9px] tracking-wider text-white/90 flex items-center gap-0.5 pointer-events-none">
              <Film className="w-2.5 h-2.5" />
              LIVE
            </span>
          )}

          {isPartnerViewing && (
            <div className="absolute top-2.5 left-2.5 px-2.5 py-1 rounded-chip bg-black/55 backdrop-blur-md text-[10px] text-white flex items-center gap-1.5 pointer-events-none">
              <span className="w-1.5 h-1.5 rounded-chip bg-accent animate-pulse" />
              <span>{partnerName || 'TA'} 正在看</span>
            </div>
          )}

          {/* WeChat-style Async Transfer Spinner / Retry Overlay */}
          {p._transferState === 'sending' && (
            <div className="absolute inset-0 bg-black/18 flex items-end justify-end p-2.5 pointer-events-none">
              <span className="px-2.5 py-1 rounded-chip bg-black/60 backdrop-blur-md text-[10px] text-white/95 flex items-center gap-1.5 shadow-xs">
                <Loader2 className="w-3 h-3 animate-spin" />
                <span>传输中…</span>
              </span>
            </div>
          )}

          {p._transferState === 'failed' && (
            <div
              onClick={(e) => e.stopPropagation()}
              className="absolute inset-0 bg-black/35 backdrop-blur-[1px] flex items-center justify-center gap-2 p-3"
            >
              <button
                type="button"
                onClick={() => onRetryUpload?.(p.id)}
                className="px-3 py-1.5 rounded-chip bg-accent text-white text-xs font-medium flex items-center gap-1.5 shadow-paper cursor-pointer"
              >
                <AlertCircle className="w-3.5 h-3.5" />
                <span>重传</span>
                <RotateCw className="w-3 h-3" />
              </button>
              <button
                type="button"
                aria-label="取消上传"
                onClick={() => onDismissFailedUpload?.(p.id)}
                className="w-7 h-7 rounded-chip bg-black/60 text-white flex items-center justify-center cursor-pointer"
              >
                <X className="w-3.5 h-3.5" />
              </button>
            </div>
          )}
        </figure>

        {/* Quiet Archival Plate Footnote Below the Photograph (Never covers the image!) */}
        {opts.showPlateCaption !== false && (
          <div
            style={{ maxWidth: boxStyle.width }}
            className="mt-2.5 flex items-baseline justify-between gap-3 px-0.5 select-none"
          >
            <span className="text-[11px] text-muted truncate">
              {noteLabel}
            </span>
            <div className="flex items-center gap-2.5 text-[10px] text-subtle shrink-0 tabular-nums">
              {p.post.likes.length > 0 && (
                <span className="inline-flex items-center gap-0.5 text-muted">
                  <Heart className="w-2.5 h-2.5 text-accent/85 fill-current" />
                  {p.post.likes.length}
                </span>
              )}
              {p.post.comments.length > 0 && (
                <span className="inline-flex items-center gap-0.5 text-muted">
                  <MessageCircle className="w-2.5 h-2.5" />
                  {p.post.comments.length}
                </span>
              )}
              <span className="tracking-wider">{dayStr}</span>
            </div>
          </div>
        )}
      </div>
    );
  };

  return (
    <main className="min-h-[100dvh] w-full pt-16 sm:pt-20 pb-28 px-4 sm:px-7 lg:px-10 xl:px-14">
      {/* Subtle top ambient gradient so floating header breathes cleanly without washing out photos */}
      <div
        aria-hidden
        className="fixed top-0 inset-x-0 h-14 z-20 pointer-events-none"
        style={{
          background:
            'linear-gradient(to bottom, var(--kd-surface) 8%, color-mix(in srgb, var(--kd-surface) 45%, transparent) 55%, transparent 100%)',
        }}
      />

      <div ref={containerRef} className="max-w-[1520px] mx-auto w-full">
        {initialLoading ? (
          <div className="grid grid-cols-2 sm:grid-cols-3 gap-8 pt-4">
            {Array.from({ length: 6 }).map((_, i) => (
              <div
                key={i}
                className="aspect-[4/3] rounded-[2px] bg-overlay animate-pulse"
                style={{ animationDelay: `${i * 70}ms` }}
              />
            ))}
          </div>
        ) : photos.length === 0 ? (
          <div className="min-h-[72dvh] flex flex-col items-center justify-center text-center px-4">
            <button
              type="button"
              onClick={onOpenUploadModal}
              className="group flex flex-col items-center gap-4 p-10 rounded-card border border-dashed border-border-strong hover:border-ink/40 transition-colors cursor-pointer"
            >
              <div className="w-11 h-11 rounded-chip bg-overlay flex items-center justify-center text-muted group-hover:text-ink transition-colors">
                <Plus className="w-5 h-5" />
              </div>
              <div className="space-y-1">
                <div className="font-editorial text-lg text-ink">放上第一张照片</div>
                <div className="text-xs text-muted">支持任意横竖构图，或直接将图片拖入画面</div>
              </div>
            </button>
          </div>
        ) : photos.length === 1 && !compactDensity ? (
          /* Single Photograph Exhibition Centerpiece */
          <div className="min-h-[74dvh] flex flex-col items-center justify-center py-4">
            {(() => {
              const p = photos[0];
              const asp = trueAspect(p.post.width, p.post.height);
              const vh = typeof window !== 'undefined' ? window.innerHeight : 840;
              const box = fitUncroppedBox(
                asp,
                Math.min(containerWidth, 820),
                Math.min(vh * 0.68, 650),
              );
              return renderEditorialPrint(
                p,
                { width: `${box.width}px`, height: `${box.height}px` },
                { eager: true, preferFullRes: true },
              );
            })()}
          </div>
        ) : compactDensity ? (
          /* =================================================================
             COMPACT OVERVIEW MODE (⊞): Magnum-Style Archival Contact Sheet
             Every photo sits 100% uncropped inside an archival slide mount!
             ================================================================= */
          <div className="space-y-14">
            {monthSections.map((sec, secIdx) => (
              <section
                key={sec.key}
                style={
                  secIdx > 0
                    ? { contentVisibility: 'auto', containIntrinsicSize: 'auto 520px' }
                    : undefined
                }
                className="space-y-5"
              >
                <div className="flex items-baseline justify-between border-b border-border pb-2.5 select-none">
                  <div className="flex items-baseline gap-2.5">
                    <span className="font-editorial text-xl text-ink tabular-nums">
                      {Number(sec.month)}月
                    </span>
                    <span className="text-xs text-subtle tracking-[0.16em] tabular-nums">
                      {sec.year}
                    </span>
                  </div>
                  <span className="text-[11px] text-subtle tracking-[0.14em] tabular-nums">
                    {String(sec.items.length).padStart(2, '0')} FRAMES
                  </span>
                </div>

                <div className="grid grid-cols-2 sm:grid-cols-3 md:grid-cols-4 xl:grid-cols-5 gap-4 sm:gap-6">
                  {sec.items.map((p, idx) => {
                    const imgUrl =
                      resolveMediaUrl(p.asset_id, assetUrls, true) ||
                      resolveMediaUrl(p.asset_id, assetUrls, false);
                    return (
                      <div
                        key={p.id}
                        role="button"
                        tabIndex={0}
                        onClick={() => onSelectPhoto(p)}
                        onKeyDown={(e) => e.key === 'Enter' && onSelectPhoto(p)}
                        className="group flex flex-col gap-2 cursor-pointer select-none outline-none"
                      >
                        <figure
                          data-photo-id={p.id}
                          style={{ backgroundColor: 'var(--kd-mat)' }}
                          className="relative aspect-[4/3] w-full rounded-[3px] border border-border group-hover:border-border-strong p-2.5 sm:p-3.5 flex items-center justify-center overflow-hidden transition-colors"
                        >
                          {imgUrl && (
                            <img
                              src={imgUrl}
                              alt={p.post.caption || ''}
                              loading={secIdx === 0 && idx < 10 ? 'eager' : 'lazy'}
                              className="max-w-full max-h-full object-contain rounded-[1px] shadow-2xs"
                            />
                          )}
                          {p._transferState === 'sending' && (
                            <span className="absolute bottom-2 right-2 px-2 py-0.5 rounded-chip bg-black/60 backdrop-blur-md text-[10px] text-white flex items-center gap-1">
                              <Loader2 className="w-2.5 h-2.5 animate-spin" />
                              <span>传输中</span>
                            </span>
                          )}
                          {p._transferState === 'failed' && (
                            <button
                              type="button"
                              onClick={(e) => {
                                e.stopPropagation();
                                onRetryUpload?.(p.id);
                              }}
                              className="absolute bottom-2 right-2 px-2 py-0.5 rounded-chip bg-accent text-[10px] text-white flex items-center gap-1 cursor-pointer"
                            >
                              <AlertCircle className="w-2.5 h-2.5" />
                              <span>重传</span>
                            </button>
                          )}
                        </figure>
                        <div className="flex items-center justify-between text-[10px] text-subtle px-0.5 tabular-nums">
                          <span>#{String(idx + 1).padStart(2, '0')}</span>
                          <span className="truncate max-w-[70%] text-muted">
                            {p.post.caption || ''}
                          </span>
                        </div>
                      </div>
                    );
                  })}
                </div>
              </section>
            ))}
          </div>
        ) : (
          /* =================================================================
             DEFAULT CURATED EDITORIAL MONOGRAPH WALL
             Sticky left architectural index + asymmetric staggered spreads
             ================================================================= */
          <div className="space-y-12 sm:space-y-16">
            {(() => {
              let runningBeat = 0;
              return monthSections.map((sec, secIdx) => {
                const composed = composeEditorialSpreads(sec.items, fieldWidth, runningBeat);
                runningBeat = composed.nextBeat;
                const { spreads } = composed;
                return (
                  <section
                    key={sec.key}
                    style={
                      secIdx > 0
                        ? { contentVisibility: 'auto', containIntrinsicSize: 'auto 620px' }
                        : undefined
                    }
                    className={
                      isLargeDesktop
                        ? 'grid grid-cols-[100px_1fr] xl:grid-cols-[108px_1fr] gap-6 xl:gap-7 items-start'
                        : 'space-y-5'
                    }
                  >
                  {/* Sticky Architectural Month Margin */}
                  <div
                    className={
                      isLargeDesktop
                        ? 'sticky top-20 pt-0.5 select-none'
                        : 'flex items-baseline justify-between border-b border-border pb-2 select-none'
                    }
                  >
                    <div className="flex items-baseline gap-1">
                      <span className="font-editorial text-2xl sm:text-3xl text-ink leading-none tabular-nums">
                        {sec.month}
                      </span>
                      <span className="text-xs text-muted">月</span>
                    </div>
                    {isLargeDesktop && <div className="w-5 h-px bg-border-strong my-2.5" />}
                    <div className="text-[10px] text-subtle tracking-[0.16em] tabular-nums">
                      {sec.year} · {String(sec.items.length).padStart(2, '0')}
                    </div>
                  </div>

                  {/* Compact Asymmetric Salon Spreads */}
                  <div className="space-y-7 sm:space-y-9 lg:space-y-10 min-w-0">
                    {spreads.map((sp, spIdx) => (
                      <div
                        key={spIdx}
                        style={{ gap: `${sp.gapPx}px` }}
                        className={`flex w-full ${sp.alignClass}`}
                      >
                        {sp.slots.map((slot) => (
                          <div
                            key={slot.photo.id}
                            style={
                              slot.offsetTop > 0
                                ? { marginTop: `${slot.offsetTop}px` }
                                : undefined
                            }
                            className="shrink-0 max-w-full"
                          >
                            {renderEditorialPrint(
                              slot.photo,
                              {
                                width: `${slot.width}px`,
                                height: `${slot.height}px`,
                              },
                              {
                                eager: secIdx === 0 && spIdx < 2,
                                preferFullRes: photos.length <= 2 || sp.slots.length === 1,
                              },
                            )}
                          </div>
                        ))}
                      </div>
                    ))}
                  </div>
                </section>
                );
              });
            })()}
          </div>
        )}

        <div ref={sentinelRef} className="h-10 flex items-center justify-center mt-8">
          {loadingMore && <span className="text-xs text-subtle">加载更多…</span>}
        </div>
      </div>

      {/* Drag Overlay */}
      <AnimatePresence>
        {dragActive && (
          <motion.div
            initial={{ opacity: 0 }}
            animate={{ opacity: 1 }}
            exit={{ opacity: 0 }}
            className="fixed inset-3 z-50 rounded-card border-2 border-dashed border-accent bg-surface/90 backdrop-blur-sm flex items-center justify-center pointer-events-none"
          >
            <p className="font-editorial text-xl text-ink">松手添加照片</p>
          </motion.div>
        )}
      </AnimatePresence>

      {/* Upload Modal */}
      <AnimatePresence>
        {uploadModalOpen && (
          <motion.div
            initial={{ opacity: 0 }}
            animate={{ opacity: 1 }}
            exit={{ opacity: 0 }}
            transition={{ duration: 0.16, ease: 'easeOut' }}
            onClick={onCloseUploadModal}
            className="fixed inset-0 z-50 bg-black/50 backdrop-blur-xs flex items-end sm:items-center justify-center p-0 sm:p-4"
          >
            <motion.form
              initial={{ y: 10, opacity: 0, scale: 0.985 }}
              animate={{ y: 0, opacity: 1, scale: 1 }}
              exit={{ y: 8, opacity: 0, scale: 0.985 }}
              transition={{ duration: 0.18, ease: [0.22, 1, 0.36, 1] }}
              onClick={(e) => e.stopPropagation()}
              onSubmit={handleUploadSubmit}
              className="w-full sm:max-w-[440px] bg-panel border border-border rounded-t-card sm:rounded-card p-5 sm:p-6 pb-safe shadow-float space-y-4 transform-gpu"
            >
              <div className="flex items-center justify-between">
                <h3 className="font-editorial text-lg text-ink">添加照片</h3>
                <button
                  type="button"
                  onClick={onCloseUploadModal}
                  className="p-1 text-muted hover:text-ink cursor-pointer"
                >
                  <X className="w-4 h-4" />
                </button>
              </div>

              {pending.length === 0 ? (
                <label className="flex flex-col items-center justify-center gap-2 w-full aspect-[16/10] rounded-input border border-dashed border-border-strong bg-surface text-muted cursor-pointer hover:border-ink/50 transition-colors">
                  <ImagePlus className="w-5 h-5 text-subtle" />
                  <span className="text-xs">选择或拖入照片（最多 {MAX_BATCH} 张）</span>
                  <input
                    type="file"
                    accept="image/*"
                    multiple
                    onChange={(e) => e.target.files && addFiles(e.target.files)}
                    className="hidden"
                  />
                </label>
              ) : (
                <div className="grid grid-cols-3 gap-2">
                  {pending.map((p) => (
                    <div
                      key={p.preview}
                      className="relative aspect-square rounded-slip overflow-hidden bg-overlay"
                    >
                      <img src={p.preview} alt="" className="w-full h-full object-cover" />
                      <button
                        type="button"
                        onClick={() => {
                          URL.revokeObjectURL(p.preview);
                          setPending((prev) => prev.filter((x) => x.preview !== p.preview));
                        }}
                        className="absolute top-1.5 right-1.5 w-5 h-5 rounded-chip bg-black/55 text-white flex items-center justify-center cursor-pointer"
                      >
                        <X className="w-3 h-3" />
                      </button>
                    </div>
                  ))}
                  {pending.length < MAX_BATCH && (
                    <label className="aspect-square rounded-slip border border-dashed border-border-strong flex items-center justify-center text-subtle hover:text-ink hover:border-ink/50 cursor-pointer transition-colors">
                      <Plus className="w-5 h-5" />
                      <input
                        type="file"
                        accept="image/*"
                        multiple
                        onChange={(e) => e.target.files && addFiles(e.target.files)}
                        className="hidden"
                      />
                    </label>
                  )}
                </div>
              )}

              {pending.length === 1 && (
                <div className="flex items-center justify-between px-3 py-2 rounded-input bg-overlay text-xs text-muted">
                  <span>实况短片（Live Photo，可选）</span>
                  <label className="text-accent cursor-pointer">
                    {liveVideo ? '已附上 ✓' : '添加视频'}
                    <input
                      type="file"
                      accept="video/mp4,video/quicktime,video/webm"
                      onChange={(e) => setLiveVideo(e.target.files?.[0] || null)}
                      className="hidden"
                    />
                  </label>
                </div>
              )}

              <div className="space-y-2">
                <input
                  type="text"
                  value={location}
                  onChange={(e) => setLocation(e.target.value)}
                  placeholder="地点（选填）"
                  className="w-full px-3 py-2 text-sm bg-surface border border-border rounded-input text-ink placeholder:text-subtle focus:outline-none focus:border-border-strong"
                />
                <textarea
                  rows={2}
                  value={caption}
                  onChange={(e) => setCaption(e.target.value)}
                  placeholder="写句记录（选填）"
                  className="w-full px-3 py-2 text-sm bg-surface border border-border rounded-input text-ink placeholder:text-subtle focus:outline-none focus:border-border-strong resize-none"
                />
              </div>

              <div className="flex justify-end gap-2 pt-1">
                <button
                  type="button"
                  onClick={() => {
                    resetUpload(true);
                    onCloseUploadModal();
                  }}
                  className="px-4 py-2 rounded-input border border-border text-xs text-muted hover:text-ink cursor-pointer"
                >
                  取消
                </button>
                <button
                  type="submit"
                  disabled={pending.length === 0}
                  className="px-5 py-2 rounded-input bg-ink text-surface text-xs font-medium disabled:opacity-40 cursor-pointer"
                >
                  {`发布${pending.length > 1 ? ` (${pending.length})` : ''}`}
                </button>
              </div>
            </motion.form>
          </motion.div>
        )}
      </AnimatePresence>
    </main>
  );
};

const FadeImg: React.FC<{ src: string; alt: string; eager?: boolean }> = ({
  src,
  alt,
  eager,
}) => {
  const [loaded, setLoaded] = useState(false);
  const imgRef = useRef<HTMLImageElement | null>(null);

  useLayoutEffect(() => {
    const el = imgRef.current;
    if (el && el.complete && el.naturalWidth > 0) {
      setLoaded(true);
    } else {
      setLoaded(false);
    }
  }, [src]);

  return (
    <img
      ref={imgRef}
      src={src}
      alt={alt}
      loading={eager ? 'eager' : 'lazy'}
      onLoad={() => setLoaded(true)}
      onError={() => setLoaded(true)}
      className={`w-full h-full object-cover transition-opacity duration-300 ease-out ${
        loaded ? 'opacity-100' : 'opacity-0'
      }`}
    />
  );
};
