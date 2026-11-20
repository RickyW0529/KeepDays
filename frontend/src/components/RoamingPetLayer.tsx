import React, { useCallback, useEffect, useLayoutEffect, useRef, useState } from 'react';
import { Gift, Image as ImageIcon, Mail } from 'lucide-react';
import type { PetFullResp, SnowflakeId } from '../types/api';

interface RoamingPetLayerProps {
  isSharedSpace: boolean;
  fridgeOpen: boolean;
  compactDensity?: boolean;
  petData: PetFullResp | null;
  agentBubble?: { pose?: string; text?: string | null } | null;
  fridgeAnchorRef: React.RefObject<HTMLButtonElement | null>;
  onOpenPetDrawer: () => void;
  onOpenOldMemoryPhoto: (photoId: SnowflakeId) => void;
}

export type PetPose =
  | 'sleeping'
  | 'in_box'
  | 'sitting'
  | 'grooming'
  | 'stretching'
  | 'walking'
  | 'peeking'
  | 'hopping';

type SurfaceAnchor =
  | { kind: 'fridge_roof' }
  | { kind: 'floor'; offsetLeft: number }
  | { kind: 'photo_ledge'; photoId: string; ratioX: number };

const STROLL_PX_PER_SEC = 21; // Slow, unhurried feline promenade (~21px/s)

export const RoamingPetLayer: React.FC<RoamingPetLayerProps> = ({
  isSharedSpace,
  fridgeOpen,
  compactDensity,
  petData,
  agentBubble,
  fridgeAnchorRef,
  onOpenPetDrawer,
  onOpenOldMemoryPhoto,
}) => {
  const actorRef = useRef<HTMLDivElement | null>(null);
  const surfaceRef = useRef<SurfaceAnchor>({ kind: 'fridge_roof' });
  const rafRef = useRef<number | null>(null);
  const timersRef = useRef<number[]>([]);

  const [pose, setPose] = useState<PetPose>('sitting');
  const [facingLeft, setFacingLeft] = useState(true);
  const [onFridgeRoof, setOnFridgeRoof] = useState(true);
  const [relocating, setRelocating] = useState(false);
  const [hovered, setHovered] = useState(false);

  const clearScheduledTimers = useCallback(() => {
    timersRef.current.forEach((id) => window.clearTimeout(id));
    timersRef.current = [];
    if (rafRef.current !== null) {
      cancelAnimationFrame(rafRef.current);
      rafRef.current = null;
    }
  }, []);

  const schedule = useCallback((fn: () => void, ms: number) => {
    const id = window.setTimeout(fn, ms);
    timersRef.current.push(id);
    return id;
  }, []);

  const getFridgeMetrics = useCallback(() => {
    const el = fridgeAnchorRef.current;
    if (!el) {
      const right = window.innerWidth - 16;
      const left = right - 64;
      const top = window.innerHeight - 118;
      const bottom = window.innerHeight - 18;
      return {
        roofX: Math.round(left + 33),
        roofY: Math.round(top + 1),
        floorBaseX: Math.round(left - 24),
        floorY: Math.round(bottom + 4),
      };
    }
    const r = el.getBoundingClientRect();
    return {
      roofX: Math.round(r.left + r.width * 0.52),
      roofY: Math.round(r.top + 1),
      floorBaseX: Math.round(r.left - 24),
      floorY: Math.round(r.bottom + 4),
    };
  }, [fridgeAnchorRef]);

  const resolveAnchorPoint = useCallback(
    (anchor: SurfaceAnchor): { x: number; y: number; valid: boolean } => {
      const fm = getFridgeMetrics();
      if (anchor.kind === 'fridge_roof') {
        return { x: fm.roofX, y: fm.roofY, valid: true };
      }
      if (anchor.kind === 'floor') {
        const minX = 28;
        const clampedX = Math.max(minX, Math.min(fm.floorBaseX, fm.floorBaseX - anchor.offsetLeft));
        return { x: Math.round(clampedX), y: fm.floorY, valid: true };
      }
      const photoEl = document.querySelector<HTMLElement>(
        `[data-photo-id="${anchor.photoId}"]`,
      );
      if (!photoEl) {
        return { x: fm.roofX, y: fm.roofY, valid: false };
      }
      const r = photoEl.getBoundingClientRect();
      if (r.top < 74 || r.top > window.innerHeight - 68 || r.width < 110) {
        return { x: fm.roofX, y: fm.roofY, valid: false };
      }
      const clampedRatio = Math.max(0.16, Math.min(0.84, anchor.ratioX));
      return {
        x: Math.round(r.left + r.width * clampedRatio),
        y: Math.round(r.top + 1),
        valid: true,
      };
    },
    [getFridgeMetrics],
  );

  const applyActorTransform = useCallback((x: number, y: number) => {
    const node = actorRef.current;
    if (node) {
      node.style.transform = `translate3d(${x.toFixed(1)}px, ${y.toFixed(1)}px, 0)`;
    }
  }, []);

  const syncSurfaceNow = useCallback(() => {
    const pt = resolveAnchorPoint(surfaceRef.current);
    if (!pt.valid) {
      // Photo scrolled out of view: softly return to fridge roof with zero diagonal drift
      clearScheduledTimers();
      surfaceRef.current = { kind: 'fridge_roof' };
      setOnFridgeRoof(true);
      setPose('sitting');
      const home = getFridgeMetrics();
      applyActorTransform(home.roofX, home.roofY);
      return;
    }
    applyActorTransform(pt.x, pt.y);
  }, [resolveAnchorPoint, clearScheduledTimers, getFridgeMetrics, applyActorTransform]);

  // Parabolic short hop between Fridge Roof <-> Adjacent Floor (0.34s arc, never drifts!)
  const runShortHop = useCallback(
    (
      fromPt: { x: number; y: number },
      toAnchor: SurfaceAnchor,
      onDone: () => void,
    ) => {
      if (rafRef.current !== null) cancelAnimationFrame(rafRef.current);
      const toPt = resolveAnchorPoint(toAnchor);
      setFacingLeft(toPt.x <= fromPt.x);
      setPose('hopping');
      setOnFridgeRoof(toAnchor.kind === 'fridge_roof');

      const durationMs = 340;
      const startTs = performance.now();

      const tick = (now: number) => {
        const rawT = Math.min(1, (now - startTs) / durationMs);
        const easeT = 1 - Math.pow(1 - rawT, 2);
        const liveTarget = resolveAnchorPoint(toAnchor);
        const curX = fromPt.x + (liveTarget.x - fromPt.x) * easeT;
        const linearY = fromPt.y + (liveTarget.y - fromPt.y) * easeT;
        const arcLift = Math.sin(Math.PI * rawT) * 20;
        applyActorTransform(curX, linearY - arcLift);

        if (rawT < 1) {
          rafRef.current = requestAnimationFrame(tick);
        } else {
          rafRef.current = null;
          surfaceRef.current = toAnchor;
          applyActorTransform(liveTarget.x, liveTarget.y);
          onDone();
        }
      };
      rafRef.current = requestAnimationFrame(tick);
    },
    [resolveAnchorPoint, applyActorTransform],
  );

  // Purely Horizontal Slow Stroll (dy === 0 relative to surface! Exact start/stop paw sync!)
  const runHorizontalStroll = useCallback(
    (
      nextAnchor:
        | { kind: 'floor'; offsetLeft: number }
        | { kind: 'photo_ledge'; photoId: string; ratioX: number },
      afterPose: PetPose,
      onComplete?: () => void,
    ) => {
      if (rafRef.current !== null) cancelAnimationFrame(rafRef.current);
      const startAnchor = surfaceRef.current;
      const startPt = resolveAnchorPoint(startAnchor);
      const endPt = resolveAnchorPoint(nextAnchor);

      if (!endPt.valid) {
        syncSurfaceNow();
        return;
      }

      const distPx = Math.abs(endPt.x - startPt.x);
      if (distPx < 10) {
        surfaceRef.current = nextAnchor;
        applyActorTransform(endPt.x, endPt.y);
        setPose(afterPose);
        onComplete?.();
        return;
      }

      setFacingLeft(endPt.x < startPt.x);
      setPose('walking');
      setOnFridgeRoof(false);

      const durationMs = Math.max(1200, (distPx / STROLL_PX_PER_SEC) * 1000);
      const startTs = performance.now();

      const tick = (now: number) => {
        const t = Math.min(1, (now - startTs) / durationMs);
        // Gentle sine ease-in-out at the very start & end (first/last 12%) while keeping steady stroll speed in middle
        const smoothT =
          t < 0.12
            ? (t * t) / (0.12 * 1.76)
            : t > 0.88
              ? 1 - ((1 - t) * (1 - t)) / (0.12 * 1.76)
              : (t - 0.06) / 0.88;
        const clampedT = Math.max(0, Math.min(1, smoothT));

        if (
          startAnchor.kind === 'floor' &&
          nextAnchor.kind === 'floor'
        ) {
          const curOffset =
            startAnchor.offsetLeft +
            (nextAnchor.offsetLeft - startAnchor.offsetLeft) * clampedT;
          surfaceRef.current = { kind: 'floor', offsetLeft: curOffset };
        } else if (
          startAnchor.kind === 'photo_ledge' &&
          nextAnchor.kind === 'photo_ledge' &&
          startAnchor.photoId === nextAnchor.photoId
        ) {
          const curRatio =
            startAnchor.ratioX +
            (nextAnchor.ratioX - startAnchor.ratioX) * clampedT;
          surfaceRef.current = {
            kind: 'photo_ledge',
            photoId: nextAnchor.photoId,
            ratioX: curRatio,
          };
        }

        const livePt = resolveAnchorPoint(surfaceRef.current);
        if (!livePt.valid) {
          rafRef.current = null;
          syncSurfaceNow();
          return;
        }
        applyActorTransform(livePt.x, livePt.y);

        if (t < 1) {
          rafRef.current = requestAnimationFrame(tick);
        } else {
          rafRef.current = null;
          surfaceRef.current = nextAnchor;
          const finalPt = resolveAnchorPoint(nextAnchor);
          applyActorTransform(finalPt.x, finalPt.y);
          setPose(afterPose);
          onComplete?.();
        }
      };

      rafRef.current = requestAnimationFrame(tick);
    },
    [resolveAnchorPoint, syncSurfaceNow, applyActorTransform],
  );

  // Soft fade relocation for distant photo ledges (eliminates 800px mid-air diagonal levitation!)
  const relocateSoftlyTo = useCallback(
    (targetAnchor: SurfaceAnchor, landedPose: PetPose, afterLand?: () => void) => {
      setRelocating(true);
      schedule(() => {
        surfaceRef.current = targetAnchor;
        setOnFridgeRoof(targetAnchor.kind === 'fridge_roof');
        setPose(landedPose);
        const pt = resolveAnchorPoint(targetAnchor);
        applyActorTransform(pt.x, pt.y);
        setRelocating(false);
        if (afterLand) {
          schedule(afterLand, 260);
        }
      }, 190);
    },
    [schedule, resolveAnchorPoint, applyActorTransform],
  );

  // Seat flush onto fridge roof on initial layout & resize
  useLayoutEffect(() => {
    if (!isSharedSpace) return;
    syncSurfaceNow();
    const el = fridgeAnchorRef.current;
    if (!el) return;
    const ro = new ResizeObserver(() => {
      if (rafRef.current === null) syncSurfaceNow();
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, [isSharedSpace, fridgeAnchorRef, syncSurfaceNow]);

  // Keep locked to physical surface during scroll, resize, or density toggle
  useEffect(() => {
    if (!isSharedSpace) return;
    const onLayoutChange = () => {
      if (rafRef.current === null) {
        syncSurfaceNow();
      }
    };
    onLayoutChange();
    window.addEventListener('scroll', onLayoutChange, { passive: true });
    window.addEventListener('resize', onLayoutChange);
    return () => {
      window.removeEventListener('scroll', onLayoutChange);
      window.removeEventListener('resize', onLayoutChange);
    };
  }, [isSharedSpace, compactDensity, syncSurfaceNow]);

  // Autonomous Calm Feline Life Choreography
  useEffect(() => {
    clearScheduledTimers();
    if (!isSharedSpace) return;

    const behavior = petData?.pet.current.behavior;
    const adopted = Boolean(petData?.pet.adopted);
    const mustStayRoof =
      !adopted ||
      behavior === 'SLEEPING_FRIDGE' ||
      behavior === 'PLAYING_BOX' ||
      behavior === 'CARRYING_OLD_PHOTO' ||
      behavior === 'ADVENTURING';

    if (mustStayRoof) {
      surfaceRef.current = { kind: 'fridge_roof' };
      setOnFridgeRoof(true);
      setFacingLeft(true);
      setPose(
        behavior === 'SLEEPING_FRIDGE'
          ? 'sleeping'
          : behavior === 'PLAYING_BOX' || petData?.pet.current.bed_style === 'cardboard_box'
            ? 'in_box'
            : 'sitting',
      );
      const fm = getFridgeMetrics();
      applyActorTransform(fm.roofX, fm.roofY);
      return;
    }

    let cancelled = false;

    const runNextRoutine = () => {
      if (cancelled) return;
      const cur = surfaceRef.current;

      // Case 1: Currently perched on Fridge Roof -> stretch, groom, hop to floor stroll, or visit a photo ledge
      if (cur.kind === 'fridge_roof') {
        const roll = Math.random();

        // 32%: Stay on Fridge Roof and groom paw or stretch lazily
        if (roll < 0.32) {
          setPose(Math.random() < 0.6 ? 'grooming' : 'stretching');
          schedule(() => {
            if (cancelled) return;
            setPose('sitting');
            schedule(runNextRoutine, 6500);
          }, 2600);
          return;
        }

        // 38%: Hop down onto the floor beside the fridge and go for a slow horizontal stroll!
        const maxWalkRoom = Math.min(280, Math.max(0, window.innerWidth - 150));
        if (roll < 0.70 && maxWalkRoom >= 90) {
          setPose('stretching');
          schedule(() => {
            if (cancelled) return;
            const fromPt = resolveAnchorPoint(surfaceRef.current);
            const landingAnchor: SurfaceAnchor = { kind: 'floor', offsetLeft: 6 };
            runShortHop(fromPt, landingAnchor, () => {
              setPose('sitting');
              schedule(() => {
                if (cancelled) return;
                const strollDist = Math.round(75 + Math.random() * (maxWalkRoom - 75));
                runHorizontalStroll(
                  { kind: 'floor', offsetLeft: strollDist },
                  Math.random() < 0.5 ? 'grooming' : 'sitting',
                  () => {
                    schedule(() => {
                      if (cancelled) return;
                      setPose('sitting');
                      schedule(runNextRoutine, 5200);
                    }, 2600);
                  },
                );
              }, 750);
            });
          }, 1100);
          return;
        }

        // 30%: Visit a wide visible photo frame ledge and stroll lazily across its top edge!
        const candidates = Array.from(
          document.querySelectorAll<HTMLElement>('[data-photo-id]'),
        ).filter((el) => {
          const r = el.getBoundingClientRect();
          return r.top >= 96 && r.top <= window.innerHeight - 120 && r.width >= 165;
        });

        if (candidates.length > 0) {
          const focusId = petData?.pet.current.focus_photo_id;
          const chosen =
            (focusId && candidates.find((n) => n.dataset.photoId === focusId)) ||
            candidates[Math.floor(Math.random() * candidates.length)];
          const photoId = chosen.dataset.photoId!;
          const startLeft = Math.random() < 0.5;
          const r0 = startLeft ? 0.22 : 0.78;
          const r1 = startLeft ? 0.72 : 0.28;

          setFacingLeft(!startLeft);
          relocateSoftlyTo(
            { kind: 'photo_ledge', photoId, ratioX: r0 },
            'sitting',
            () => {
              if (cancelled) return;
              schedule(() => {
                if (cancelled) return;
                runHorizontalStroll(
                  { kind: 'photo_ledge', photoId, ratioX: r1 },
                  'peeking',
                  () => {
                    schedule(runNextRoutine, 5800);
                  },
                );
              }, 900);
            },
          );
          return;
        }

        schedule(runNextRoutine, 7000);
        return;
      }

      // Case 2: Currently on the Floor -> either stroll a bit further or stroll slowly back & hop onto Fridge Roof
      if (cur.kind === 'floor') {
        if (Math.random() < 0.35 && cur.offsetLeft < 160 && window.innerWidth > 500) {
          const nextOffset = Math.min(260, cur.offsetLeft + 70 + Math.random() * 60);
          runHorizontalStroll({ kind: 'floor', offsetLeft: nextOffset }, 'sitting', () => {
            schedule(runNextRoutine, 4800);
          });
          return;
        }
        // Stroll leisurely all the way back to the base of the fridge, then hop up onto the roof!
        runHorizontalStroll({ kind: 'floor', offsetLeft: 4 }, 'sitting', () => {
          schedule(() => {
            if (cancelled) return;
            const floorPt = resolveAnchorPoint(surfaceRef.current);
            runShortHop(floorPt, { kind: 'fridge_roof' }, () => {
              setFacingLeft(true);
              setPose(
                petData?.pet.current.bed_style === 'cardboard_box' ? 'in_box' : 'sitting',
              );
              schedule(runNextRoutine, 8500);
            });
          }, 550);
        });
        return;
      }

      // Case 3: Currently on a Photo Ledge -> stroll a few slow steps or softly return to Fridge Roof
      if (cur.kind === 'photo_ledge') {
        if (Math.random() < 0.4) {
          const nextRatio = cur.ratioX > 0.5 ? 0.26 + Math.random() * 0.16 : 0.58 + Math.random() * 0.18;
          runHorizontalStroll(
            { kind: 'photo_ledge', photoId: cur.photoId, ratioX: nextRatio },
            Math.random() < 0.55 ? 'peeking' : 'grooming',
            () => {
              schedule(() => {
                if (!cancelled) setPose('sitting');
                schedule(runNextRoutine, 5000);
              }, 2800);
            },
          );
          return;
        }

        relocateSoftlyTo(
          { kind: 'fridge_roof' },
          petData?.pet.current.bed_style === 'cardboard_box' ? 'in_box' : 'sitting',
          () => {
            setFacingLeft(true);
            schedule(runNextRoutine, 8500);
          },
        );
      }
    };

    // Start calm life loop after initial settle
    schedule(runNextRoutine, 3800);

    return () => {
      cancelled = true;
      clearScheduledTimers();
    };
  }, [
    isSharedSpace,
    petData,
    clearScheduledTimers,
    schedule,
    getFridgeMetrics,
    applyActorTransform,
    resolveAnchorPoint,
    runShortHop,
    runHorizontalStroll,
    relocateSoftlyTo,
  ]);

  if (!isSharedSpace) return null;

  const pet = petData?.pet;
  const unadopted = !pet || !pet.adopted;
  const isAdventuring = pet?.current.behavior === 'ADVENTURING';
  const carryingOldId = pet?.current.carrying_old_photo_id;
  const hasUnreadLetter = petData?.skills.has_unread_board_letter;

  return (
    <div
      className={`fixed inset-0 z-[32] pointer-events-none overflow-hidden transition-opacity duration-200 ${
        fridgeOpen ? 'opacity-0' : 'opacity-100'
      }`}
    >
      <div
        ref={actorRef}
        className={`absolute top-0 left-0 w-0 h-0 will-change-transform transition-opacity duration-200 ${
          relocating ? 'opacity-0' : 'opacity-100'
        }`}
      >
        <div
          className={`absolute bottom-0 left-0 -translate-x-1/2 ${
            fridgeOpen ? 'pointer-events-none' : 'pointer-events-auto'
          }`}
        >
          {unadopted ? (
            <button
              type="button"
              onClick={onOpenPetDrawer}
              title="领养共享伙伴"
              className="group flex flex-col items-center cursor-pointer"
            >
              <span className="mb-1 px-2 py-0.5 rounded-chip bg-panel/95 border border-border text-[10px] text-ink opacity-0 group-hover:opacity-100 transition-opacity whitespace-nowrap shadow-xs">
                领养伙伴
              </span>
              <div className="w-7 h-6 rounded-[4px] bg-paper-oat border border-border-strong text-accent flex items-center justify-center shadow-paper hover:-translate-y-0.5 transition-transform">
                <Gift className="w-3.5 h-3.5" />
              </div>
            </button>
          ) : isAdventuring ? (
            <button
              type="button"
              onClick={onOpenPetDrawer}
              title={`${pet.name} 外出散步中`}
              className="group flex flex-col items-center cursor-pointer"
            >
              <span className="mb-1 px-2 py-0.5 rounded-chip bg-panel/95 border border-border text-[10px] text-muted opacity-0 group-hover:opacity-100 transition-opacity whitespace-nowrap">
                散步中
              </span>
              <div className="w-9 h-2 rounded-full bg-paper-oat border border-border-strong shadow-2xs" />
            </button>
          ) : (
            <div className="relative flex flex-col items-center">
              {agentBubble?.text ? (
                <button
                  type="button"
                  onClick={onOpenPetDrawer}
                  className="mb-1 max-w-[168px] px-2.5 py-1 rounded-slip bg-paper-oat border border-accent/50 text-[10px] text-ink shadow-paper leading-snug text-left cursor-pointer hover:-translate-y-0.5 transition-transform"
                >
                  {agentBubble.text}
                </button>
              ) : (
                hovered &&
                !carryingOldId && (
                  <span className="mb-0.5 px-2 py-0.5 rounded-chip bg-panel/95 border border-border text-[10px] text-ink shadow-xs whitespace-nowrap pointer-events-none">
                    {pet.name} · 点我私语
                  </span>
                )
              )}

              {carryingOldId && (
                <button
                  type="button"
                  onClick={(e) => {
                    e.stopPropagation();
                    onOpenOldMemoryPhoto(carryingOldId);
                  }}
                  title="查看它翻出的旧照片"
                  className="mb-1 px-2 py-0.5 rounded-[3px] bg-paper-white border border-accent/60 text-[10px] text-ink shadow-paper flex items-center gap-1 cursor-pointer whitespace-nowrap hover:-translate-y-0.5 transition-transform"
                >
                  <ImageIcon className="w-2.5 h-2.5 text-accent" />
                  <span>旧回忆</span>
                </button>
              )}

              {!carryingOldId && hasUnreadLetter && onFridgeRoof && (
                <button
                  type="button"
                  onClick={onOpenPetDrawer}
                  title="留言板有新字条"
                  className="mb-0.5 w-5 h-4 rounded-[2px] bg-paper-oat border border-accent/50 text-accent flex items-center justify-center shadow-2xs cursor-pointer"
                >
                  <Mail className="w-2.5 h-2.5" />
                </button>
              )}

              <button
                type="button"
                aria-label={`${pet.name} 宠物互动`}
                onClick={onOpenPetDrawer}
                onPointerEnter={() => setHovered(true)}
                onPointerLeave={() => setHovered(false)}
                style={{ transform: facingLeft ? 'scaleX(1)' : 'scaleX(-1)' }}
                className={`${
                  pose === 'sleeping'
                    ? 'w-9 h-5 sm:w-10 sm:h-5'
                    : pose === 'walking' || pose === 'stretching' || pose === 'peeking'
                      ? 'w-11 h-9 sm:w-12 sm:h-10'
                      : 'w-10 h-9 sm:w-11 sm:h-10'
                } flex items-end justify-center cursor-pointer focus:outline-none`}
              >
                <PetIllustrationSvg
                  species={pet.species}
                  pose={(agentBubble?.pose as PetPose) || pose}
                  redScarf={pet.current.equipped_accessory === 'red_scarf'}
                  strawHat={pet.current.equipped_accessory === 'straw_hat'}
                />
              </button>
            </div>
          )}
        </div>
      </div>
    </div>
  );
};

export interface PetSvgProps {
  species: string;
  pose: PetPose;
  redScarf: boolean;
  strawHat: boolean;
}

function getSpeciesPalette(species: string) {
  switch (species) {
    case 'SHIBA':
      return {
        coat: '#C6925B',
        accent: '#F3EAD9',
        earInner: '#A87340',
        rim: 'rgba(255, 248, 236, 0.28)',
        eye: '#261E1A',
        pupil: '#FAF6F0',
      };
    case 'OTTER':
      return {
        coat: '#6B5446',
        accent: '#E5DBD0',
        earInner: '#4F3C31',
        rim: 'rgba(255, 248, 236, 0.28)',
        eye: '#1D1815',
        pupil: '#FAF6F0',
      };
    case 'BUNNY':
      return {
        coat: '#ECE7DD',
        accent: '#FAF8F4',
        earInner: '#DFB8AF',
        rim: 'rgba(42, 36, 30, 0.28)',
        eye: '#4B2E2B',
        pupil: '#FAF6F0',
      };
    case 'BLACK_CAT':
    default:
      return {
        coat: '#1B1917',
        accent: '#2B2825',
        earInner: '#4C3733',
        rim: 'rgba(250, 242, 228, 0.38)',
        eye: '#EAB96A',
        pupil: '#181614',
      };
  }
}

export const PetIllustrationSvg: React.FC<PetSvgProps> = ({
  species,
  pose,
  redScarf,
  strawHat,
}) => {
  const pal = getSpeciesPalette(species);
  const isBunny = species === 'BUNNY';
  const isOtter = species === 'OTTER';
  const isShiba = species === 'SHIBA';

  const renderEars = (cx: number, topY: number, scale = 1) => {
    if (isBunny) {
      return (
        <g>
          <path
            d={`M${cx - 4.5 * scale} ${topY + 3} C${cx - 6.5 * scale} ${topY - 9 * scale}, ${cx - 1.5 * scale} ${topY - 9 * scale}, ${cx - 1.2 * scale} ${topY + 2} Z`}
            fill={pal.coat}
            stroke={pal.rim}
            strokeWidth="0.8"
          />
          <path
            d={`M${cx + 1.2 * scale} ${topY + 2} C${cx + 1.5 * scale} ${topY - 9 * scale}, ${cx + 6.5 * scale} ${topY - 9 * scale}, ${cx + 4.5 * scale} ${topY + 3} Z`}
            fill={pal.coat}
            stroke={pal.rim}
            strokeWidth="0.8"
          />
        </g>
      );
    }
    if (isOtter) {
      return (
        <g>
          <circle cx={cx - 5.5 * scale} cy={topY + 2} r={2.2 * scale} fill={pal.coat} stroke={pal.rim} strokeWidth="0.7" />
          <circle cx={cx + 5.5 * scale} cy={topY + 2} r={2.2 * scale} fill={pal.coat} stroke={pal.rim} strokeWidth="0.7" />
        </g>
      );
    }
    return (
      <g>
        <path
          d={`M${cx - 6.2 * scale} ${topY + 3.5} L${cx - 4.5 * scale} ${topY - 4.2 * scale} L${cx - 0.8 * scale} ${topY + 1.8} Z`}
          fill={pal.coat}
          stroke={pal.rim}
          strokeWidth="0.8"
          strokeLinejoin="round"
        />
        <path
          d={`M${cx + 0.8 * scale} ${topY + 1.8} L${cx + 4.5 * scale} ${topY - 4.2 * scale} L${cx + 6.2 * scale} ${topY + 3.5} Z`}
          fill={pal.coat}
          stroke={pal.rim}
          strokeWidth="0.8"
          strokeLinejoin="round"
        />
        <polygon
          points={`${cx - 5 * scale},${topY + 2.2} ${cx - 4.2 * scale},${topY - 2.1 * scale} ${cx - 2 * scale},${topY + 1.8}`}
          fill={pal.earInner}
        />
        <polygon
          points={`${cx + 2 * scale},${topY + 1.8} ${cx + 4.2 * scale},${topY - 2.1 * scale} ${cx + 5 * scale},${topY + 2.2}`}
          fill={pal.earInner}
        />
      </g>
    );
  };

  const renderEyes = (cx: number, cy: number) => (
    <g className="kd-eye-blink">
      <ellipse cx={cx - 2.7} cy={cy} rx="1.55" ry="1.45" fill={pal.eye} />
      <ellipse cx={cx + 2.5} cy={cy} rx="1.55" ry="1.45" fill={pal.eye} />
      <circle cx={cx - 2.7} cy={cy} r="0.65" fill={pal.pupil} />
      <circle cx={cx + 2.5} cy={cy} r="0.65" fill={pal.pupil} />
    </g>
  );

  // 1. Curled Loaf Sleeping Flush on Fridge Top
  if (pose === 'sleeping') {
    return (
      <svg viewBox="6 15 36 19" className="w-full h-full overflow-visible">
        <g className="kd-sleep-loaf">
          <path
            d="M9 33 C8 23, 16 18, 26 18 C36 18, 41 24, 39 33 Z"
            fill={pal.coat}
            stroke={pal.rim}
            strokeWidth="0.9"
          />
          <circle cx="15.5" cy="24.5" r="6.2" fill={pal.coat} stroke={pal.rim} strokeWidth="0.85" />
          {renderEars(15.5, 19, 0.85)}
          <path d="M12.2 25 Q13.6 26.2 15 25" stroke={pal.eye} strokeWidth="1.1" fill="none" strokeLinecap="round" />
          <path d="M16.4 25 Q17.8 26.2 19.2 25" stroke={pal.eye} strokeWidth="1.1" fill="none" strokeLinecap="round" />
          <path
            d="M37 31 C42 29, 42 24, 35 23"
            stroke={pal.coat}
            strokeWidth="3.2"
            strokeLinecap="round"
            fill="none"
          />
          {redScarf && (
            <path d="M17 28.5 Q21 30.5 24.5 27.5" stroke="#B85D43" strokeWidth="2.4" fill="none" strokeLinecap="round" />
          )}
        </g>
      </svg>
    );
  }

  // 2. Peeking Out of Kraft Cardboard Box on Fridge Top
  if (pose === 'in_box') {
    return (
      <svg viewBox="0 0 48 40" className="w-full h-8.5 overflow-visible">
        {renderEars(24, 10, 0.95)}
        <ellipse cx="24" cy="18.5" rx="8.8" ry="6.8" fill={pal.coat} stroke={pal.rim} strokeWidth="0.85" />
        {renderEyes(24, 17.8)}
        <rect
          x="9"
          y="20.5"
          width="30"
          height="18.5"
          rx="2"
          fill="#C9A982"
          stroke="rgba(30,24,18,0.35)"
          strokeWidth="1"
        />
        <path d="M9 25.5 L39 25.5" stroke="rgba(30,24,18,0.22)" strokeWidth="1" />
        <path d="M7.5 20.5 L12 18 L24 20.5 L36 18 L40.5 20.5" fill="#D8BA95" stroke="rgba(30,24,18,0.28)" strokeWidth="0.8" />
      </svg>
    );
  }

  // 3. Feline "Big Stretch" Pose (Front paws extended forward, arched back, raised tail!)
  if (pose === 'stretching') {
    return (
      <svg viewBox="0 0 54 44" className="w-full h-full overflow-visible">
        {/* High happy tail */}
        <path
          d="M39 23 C44 15, 44 9, 40 7"
          stroke={pal.coat}
          strokeWidth="3"
          strokeLinecap="round"
          fill="none"
          className="kd-tail-sway"
        />
        {/* Sloping stretched spine: low chest at x=18, raised rump at x=38 */}
        <path
          d="M16 36 C18 31, 24 27, 32 22 C37 20, 41 24, 40 31 L39 43 L35.5 43 L35.5 32 C30 33, 24 37, 19 39 L10 43 L9 40.5 Z"
          fill={pal.coat}
          stroke={pal.rim}
          strokeWidth="0.85"
          strokeLinejoin="round"
        />
        {/* Extended front paws */}
        <path d="M18 37 L9.5 42.5" stroke={pal.coat} strokeWidth="2.8" strokeLinecap="round" />
        <path d="M20 38 L12.5 42.8" stroke={pal.coat} strokeWidth="2.6" strokeLinecap="round" />
        {/* Head lowered blissfully */}
        <g>
          {renderEars(15.5, 23.5, 0.9)}
          <ellipse cx="15.5" cy="29.5" rx="6.6" ry="5.8" fill={pal.coat} stroke={pal.rim} strokeWidth="0.85" />
          <path d="M12.2 29.8 Q13.5 31 14.8 29.8" stroke={pal.eye} strokeWidth="1.1" fill="none" strokeLinecap="round" />
          <path d="M16.2 29.8 Q17.5 31 18.8 29.8" stroke={pal.eye} strokeWidth="1.1" fill="none" strokeLinecap="round" />
        </g>
        {redScarf && (
          <path d="M15 34 Q19.5 36 23 33" stroke="#B85D43" strokeWidth="2.5" strokeLinecap="round" fill="none" />
        )}
      </svg>
    );
  }

  // 4. Upright Sitting OR Paw-Grooming Pose
  if (pose === 'sitting' || pose === 'grooming') {
    const isGrooming = pose === 'grooming';
    return (
      <svg viewBox="0 0 48 44" className="w-full h-full overflow-visible">
        <path
          d={isShiba ? 'M33 38 C40 35, 41 27, 36 25' : isBunny ? 'M34 39 C38 39, 39 35, 35 35' : 'M33 40 C42 38, 44 26, 39 19'}
          stroke={pal.coat}
          strokeWidth={isBunny ? '4' : '3.1'}
          strokeLinecap="round"
          fill="none"
          className="kd-tail-sway"
        />
        <path
          d="M17 43 C15.5 35, 16.5 25, 21 22 C25.5 20, 32 25, 34 34 C35 39, 34 43, 30 43 Z"
          fill={pal.coat}
          stroke={pal.rim}
          strokeWidth="0.9"
        />
        {(isShiba || isOtter) && (
          <ellipse cx="21.5" cy="33" rx="3.8" ry="6" fill={pal.accent} opacity="0.88" />
        )}

        {/* Front paws: when grooming, left paw lifts to cheek! */}
        {isGrooming ? (
          <>
            <path
              d="M18.5 31 C14.5 28, 13.5 23.5, 15.8 21.2"
              stroke={pal.coat}
              strokeWidth="3"
              strokeLinecap="round"
              fill="none"
              className="kd-groom-paw"
            />
            <ellipse cx="23.5" cy="42.5" rx="2.5" ry="1.4" fill={pal.coat} stroke={pal.rim} strokeWidth="0.7" />
          </>
        ) : (
          <>
            <ellipse cx="19" cy="42.5" rx="2.6" ry="1.4" fill={pal.coat} stroke={pal.rim} strokeWidth="0.7" />
            <ellipse cx="23.5" cy="42.5" rx="2.4" ry="1.4" fill={pal.coat} stroke={pal.rim} strokeWidth="0.7" />
          </>
        )}

        <g className={isGrooming ? 'kd-groom-head' : undefined}>
          {renderEars(21, 10.5, 0.95)}
          <ellipse cx="21" cy="17" rx="7.2" ry="6.4" fill={pal.coat} stroke={pal.rim} strokeWidth="0.9" />
          {isGrooming ? (
            <>
              <path d="M16.8 17.2 Q18.2 18.4 19.6 17.2" stroke={pal.eye} strokeWidth="1.1" fill="none" strokeLinecap="round" />
              <path d="M21 17.2 Q22.4 18.4 23.8 17.2" stroke={pal.eye} strokeWidth="1.1" fill="none" strokeLinecap="round" />
            </>
          ) : (
            renderEyes(20, 17)
          )}
          {strawHat && (
            <g>
              <ellipse cx="21" cy="10.5" rx="8.8" ry="2" fill="#DEC08B" />
              <path d="M15.5 10.5 Q21 5.5 26.5 10.5 Z" fill="#DEC08B" />
            </g>
          )}
        </g>

        {redScarf && (
          <g>
            <path d="M16 22.8 Q21.5 25.2 26.5 22.4" stroke="#B85D43" strokeWidth="2.8" strokeLinecap="round" fill="none" />
            <path d="M19.5 24 L18.2 29.5" stroke="#B85D43" strokeWidth="2.3" strokeLinecap="round" />
          </g>
        )}
      </svg>
    );
  }

  // 5. Mid-Hop Tucked Pose (0.34s Fridge <-> Floor Jump)
  if (pose === 'hopping') {
    return (
      <svg viewBox="0 0 52 44" className="w-full h-full overflow-visible">
        <path
          d="M38 25 C45 21, 47 15, 44 11"
          stroke={pal.coat}
          strokeWidth="3"
          strokeLinecap="round"
          fill="none"
        />
        <path
          d="M15 26 C16 20, 24 18.5, 32 20.5 C38 22, 40 26, 38 30 C35 33, 23 33, 15 26 Z"
          fill={pal.coat}
          stroke={pal.rim}
          strokeWidth="0.9"
        />
        <path d="M18 30 L14 35" stroke={pal.coat} strokeWidth="2.8" strokeLinecap="round" />
        <path d="M35 30 L39 35" stroke={pal.coat} strokeWidth="2.8" strokeLinecap="round" />
        {renderEars(15.5, 13, 0.92)}
        <ellipse cx="15.5" cy="19.5" rx="6.8" ry="6" fill={pal.coat} stroke={pal.rim} strokeWidth="0.9" />
        {renderEyes(14.2, 19.5)}
      </svg>
    );
  }

  // 6. Leisurely 4-Beat Horizontal Stroll OR Curious Photo-Peeking Pose
  const isWalking = pose === 'walking';
  const isPeeking = pose === 'peeking';

  return (
    <svg viewBox="0 0 54 44" className="w-full h-full overflow-visible">
      <g className={isWalking ? 'kd-stroll-body' : undefined}>
        {/* Relaxed trailing S-curve tail */}
        <path
          d={isShiba ? 'M39 27 C46 22, 45 15, 40 16' : 'M39 27 C47 24, 49 16, 46 11'}
          stroke={pal.coat}
          strokeWidth="3"
          strokeLinecap="round"
          fill="none"
          className="kd-tail-sway"
        />

        {/* Back far leg & front far leg (Phase B) */}
        <path
          d={isPeeking ? 'M34 34 L33.5 43' : 'M33.5 33.5 Q33 38.5 32.5 43'}
          stroke={pal.coat}
          strokeWidth="2.7"
          strokeLinecap="round"
          fill="none"
          opacity="0.82"
          className={isWalking ? 'kd-paw-b' : undefined}
        />
        <path
          d={isPeeking ? 'M23 36 L22.5 43' : 'M23.5 33.5 Q23 38.5 22.5 43'}
          stroke={pal.coat}
          strokeWidth="2.7"
          strokeLinecap="round"
          fill="none"
          opacity="0.82"
          className={isWalking ? 'kd-paw-a' : undefined}
        />

        {/* Supple feline torso (lowers chest slightly when peeking down into a photo!) */}
        <path
          d={
            isPeeking
              ? 'M15 32 C16 26, 23 24, 31 23.5 C37 23.5, 40 26.5, 39.5 31.5 C39 34.8, 34 35.5, 27 35.8 C20.5 36, 15.8 35, 15 32 Z'
              : 'M16 28.5 C16.5 22.5, 23.5 21, 31 21.8 C37 22.5, 40.5 25.5, 40 30.5 C39.5 34.2, 34.5 35, 27.5 34.8 C21 34.6, 16.5 32.8, 16 28.5 Z'
          }
          fill={pal.coat}
          stroke={pal.rim}
          strokeWidth="0.9"
        />

        {/* Back near leg & front near leg (Phase A / hooked over ledge when peeking) */}
        <path
          d={isPeeking ? 'M37.5 33.5 L37 43' : 'M37.5 33 Q37 38.5 36.5 43'}
          stroke={pal.coat}
          strokeWidth="2.9"
          strokeLinecap="round"
          fill="none"
          className={isWalking ? 'kd-paw-a' : undefined}
        />
        <path
          d={isPeeking ? 'M19 35.5 Q17.8 40.5 17.5 44.2' : 'M19.5 33 Q18.8 38.5 18.2 43'}
          stroke={pal.coat}
          strokeWidth="2.9"
          strokeLinecap="round"
          fill="none"
          className={isWalking ? 'kd-paw-b' : undefined}
        />

        {/* Head: level & unhurried while strolling; angled curiously downward when peeking at photo */}
        <g
          style={
            isPeeking
              ? { transform: 'translate(0.5px, 4.5px) rotate(-16deg)', transformOrigin: '17px 23px' }
              : undefined
          }
        >
          {renderEars(16.5, 14.5, 0.92)}
          <ellipse cx="16.5" cy="21" rx="7" ry="6.2" fill={pal.coat} stroke={pal.rim} strokeWidth="0.9" />
          {renderEyes(15.1, 21)}
          {strawHat && (
            <g>
              <ellipse cx="16.5" cy="14.5" rx="8.5" ry="2" fill="#DEC08B" />
              <path d="M11 14.5 Q16.5 9.5 22 14.5 Z" fill="#DEC08B" />
            </g>
          )}
        </g>

        {redScarf && (
          <g style={isPeeking ? { transform: 'translateY(3.5px)' } : undefined}>
            <path d="M14.2 25.8 Q19.2 28.8 23.8 25.2" stroke="#B85D43" strokeWidth="2.8" strokeLinecap="round" fill="none" />
            <path d="M18.2 27.2 L17 32.5" stroke="#B85D43" strokeWidth="2.3" strokeLinecap="round" />
          </g>
        )}
      </g>
    </svg>
  );
};
