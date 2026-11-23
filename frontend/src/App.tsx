import React, { Suspense, lazy, useCallback, useEffect, useRef, useState } from 'react';
import { AnimatePresence } from 'framer-motion';
import { AuthScreen } from './components/AuthScreen';
import { FridgeSystem, type FridgeStage } from './components/FridgeSystem';
import { PhotoWallCanvas, type QueuedPhotoUploadItem } from './components/PhotoWallCanvas';
import { PlatformToolbox, type ToolboxTab } from './components/PlatformToolbox';
import { RoamingPetLayer } from './components/RoamingPetLayer';
import { toast, Toaster } from './components/Toaster';
import { TopFloatingBar } from './components/TopFloatingBar';

const PetDrawer = lazy(() =>
  import('./components/PetDrawer').then((m) => ({ default: m.PetDrawer })),
);
const PhotoMomentsModal = lazy(() =>
  import('./components/PhotoMomentsModal').then((m) => ({ default: m.PhotoMomentsModal })),
);
const ProfileDrawer = lazy(() =>
  import('./components/ProfileDrawer').then((m) => ({ default: m.ProfileDrawer })),
);
import { useBeijingAmbient } from './hooks/useBeijingAmbient';
import { useSpaceRealtime } from './hooks/useSpaceRealtime';
import {
  api,
  getAccessToken,
  getSavedSpaceId,
  setAccessToken,
  setSavedSpaceId,
} from './lib/apiClient';
import {
  ASSET_KIND,
  compressAndUploadImage,
  predecodeImageOffscreen,
  uploadRawMediaBlob,
} from './lib/mediaProcessor';
import type {
  AssetUrlMap,
  AuthSessionResp,
  FridgeBundleResp,
  InvitationDto,
  PetFullResp,
  PhotoCardDto,
  SnowflakeId,
  SpaceDto,
  UserMap,
  UserSummaryDto,
} from './types/api';

interface PendingPhotoJob {
  tempPhotoId: SnowflakeId;
  tempAssetId: SnowflakeId;
  spaceId: SnowflakeId;
  file: File;
  preview: string;
  caption: string;
  location: string;
  liveVideo: File | null;
}

export const App: React.FC = () => {
  const [booting, setBooting] = useState(true);
  const [currentUser, setCurrentUser] = useState<UserSummaryDto | null>(null);
  const [spaces, setSpaces] = useState<SpaceDto[]>([]);
  const [activeSpaceId, setActiveSpaceId] = useState<SnowflakeId | null>(null);
  const [pendingInvites, setPendingInvites] = useState<InvitationDto[]>([]);

  // Global hydrated dictionaries
  const [usersMap, setUsersMap] = useState<UserMap>({});
  const [assetUrls, setAssetUrls] = useState<AssetUrlMap>({});

  // Photo Wall state
  const [photos, setPhotos] = useState<PhotoCardDto[]>([]);
  const [nextBeforeTakenAt, setNextBeforeTakenAt] = useState<number | null>(null);
  const [nextBeforeId, setNextBeforeId] = useState<string | null>(null);
  const [loadingMorePhotos, setLoadingMorePhotos] = useState(false);
  const [wallLoading, setWallLoading] = useState(true);
  const [compactDensity, setCompactDensity] = useState(false);
  const [uploadModalOpen, setUploadModalOpen] = useState(false);
  const [selectedPhoto, setSelectedPhoto] = useState<PhotoCardDto | null>(null);

  // Fridge, Toolbox, Profile & Shared Pet state
  const [fridgeStage, setFridgeStage] = useState<FridgeStage>('collapsed');
  const [lastExpandedStage, setLastExpandedStage] = useState<Exclude<FridgeStage, 'collapsed'>>(
    () => {
      const saved = localStorage.getItem('kd_fridge_last_stage');
      return saved === 'split_board' || saved === 'split_menu' || saved === 'door'
        ? saved
        : 'door';
    },
  );
  const [fridgeBundle, setFridgeBundle] = useState<FridgeBundleResp | null>(null);
  const [petResp, setPetResp] = useState<PetFullResp | null>(null);
  const [petDrawerOpen, setPetDrawerOpen] = useState(false);
  const [petAgentBubble, setPetAgentBubble] = useState<{
    pose?: string;
    text?: string | null;
  } | null>(null);
  const [toolboxOpen, setToolboxOpen] = useState(false);
  const [toolboxTab, setToolboxTab] = useState<ToolboxTab>('models');
  const [toolboxRefreshSeq, setToolboxRefreshSeq] = useState(0);
  const [profileDrawerUid, setProfileDrawerUid] = useState<SnowflakeId | null>(null);

  const bubbleTimerRef = useRef<number | null>(null);
  const fridgeAnchorRef = useRef<HTMLButtonElement | null>(null);
  const pendingPhotoJobsRef = useRef<Map<SnowflakeId, PendingPhotoJob>>(new Map());
  // Tracks real photo IDs currently being finalized by our own background upload so SSE PHOTO_CREATED doesn't duplicate
  const recentlyUploadedRealIdsRef = useRef<Set<SnowflakeId>>(new Set());

  const handleChangeFridgeStage = useCallback((next: FridgeStage) => {
    if (next !== 'collapsed') {
      setLastExpandedStage(next);
      localStorage.setItem('kd_fridge_last_stage', next);
    }
    setFridgeStage(next);
  }, []);

  const handleOpenFridge = useCallback(() => {
    setFridgeStage(lastExpandedStage);
  }, [lastExpandedStage]);

  const activeSpace = spaces.find((s) => s.id === activeSpaceId) || spaces[0] || null;
  const ambient = useBeijingAmbient(Boolean(activeSpace?.is_shared));

  const mergeHydrated = useCallback((u?: UserMap, a?: AssetUrlMap) => {
    if (u) setUsersMap((prev) => ({ ...prev, ...u }));
    if (a) setAssetUrls((prev) => ({ ...prev, ...a }));
  }, []);

  const loadSpacesAndInvites = useCallback(
    async (preferredSpaceId?: SnowflakeId) => {
      const [spRes, invRes, meRes] = await Promise.all([
        api.listSpaces(),
        api.listPendingInvitations(),
        api.getMyProfile(),
      ]);
      setCurrentUser(meRes.user);
      setSpaces(spRes.spaces);
      setPendingInvites(invRes.invitations);
      mergeHydrated(
        { ...spRes.users, ...invRes.users, [meRes.user.id]: meRes.user },
        { ...spRes.asset_urls, ...invRes.asset_urls, ...meRes.asset_urls },
      );

      const saved = preferredSpaceId || getSavedSpaceId();
      const validSaved = spRes.spaces.find((s) => s.id === saved);
      const chosenId = validSaved ? validSaved.id : spRes.spaces[0]?.id || null;
      if (chosenId) {
        setActiveSpaceId(chosenId);
        setSavedSpaceId(chosenId);
      }
    },
    [mergeHydrated],
  );

  // Initial session boot
  useEffect(() => {
    const boot = async () => {
      if (!getAccessToken()) {
        setBooting(false);
        return;
      }
      try {
        await loadSpacesAndInvites();
      } catch {
        setAccessToken(null);
        setCurrentUser(null);
      } finally {
        setBooting(false);
      }
    };
    boot();

    const onUnauth = () => {
      setCurrentUser(null);
      setSpaces([]);
    };
    const onOpenUpload = () => setUploadModalOpen(true);

    window.addEventListener('kd:unauthorized', onUnauth);
    window.addEventListener('kd:open-upload', onOpenUpload);
    return () => {
      window.removeEventListener('kd:unauthorized', onUnauth);
      window.removeEventListener('kd:open-upload', onOpenUpload);
    };
  }, [loadSpacesAndInvites]);

  const syncFridgePreservingOptimistic = useCallback(
    async (spaceId: SnowflakeId) => {
      try {
        const fresh = await api.getFridgeBundle(spaceId);
        mergeHydrated(fresh.users, fresh.asset_urls);
        setFridgeBundle((prev) => {
          if (!prev) return fresh;
          const pendingNotes = prev.board.data.notes.filter((n) => n._transferState === 'sending');
          const pendingTodos = prev.board.data.todos.filter((t) => t._transferState === 'sending');
          if (pendingNotes.length === 0 && pendingTodos.length === 0) return fresh;
          return {
            ...fresh,
            board: {
              ...fresh.board,
              data: {
                notes: [...pendingNotes, ...fresh.board.data.notes],
                todos: [...pendingTodos, ...fresh.board.data.todos],
              },
            },
          };
        });
      } catch {
        // ignore transient errors
      }
    },
    [mergeHydrated],
  );

  const loadSpaceData = useCallback(
    async (space: SpaceDto) => {
      const [wallRes, fRes] = await Promise.all([
        api.listPhotos(space.id, null, null, 36),
        api.getFridgeBundle(space.id),
      ]);
      setPhotos((prev) => {
        // Preserve any local optimistic uploading/failed photos for this space
        const localOptimistic = prev.filter(
          (p) => p.space_id === space.id && String(p.id).startsWith('temp_'),
        );
        const serverIds = new Set(wallRes.photos.map((p) => p.id));
        return [
          ...localOptimistic.filter((p) => !serverIds.has(p.id)),
          ...wallRes.photos,
        ];
      });
      setNextBeforeTakenAt(wallRes.next_before_taken_at || null);
      setNextBeforeId(wallRes.next_before_id || null);
      setFridgeBundle(fRes);
      mergeHydrated(
        { ...wallRes.users, ...fRes.users },
        { ...wallRes.asset_urls, ...fRes.asset_urls },
      );

      if (space.is_shared) {
        const pRes = await api.getPet(space.id);
        setPetResp(pRes);
        mergeHydrated(pRes.users, pRes.asset_urls);
      } else {
        setPetResp(null);
      }
    },
    [mergeHydrated],
  );

  useEffect(() => {
    if (!activeSpace) return;
    setFridgeStage('collapsed');
    setSelectedPhoto(null);
    setWallLoading(true);
    setPhotos([]);
    loadSpaceData(activeSpace)
      .catch(() => {})
      .finally(() => setWallLoading(false));
  }, [activeSpace?.id, loadSpaceData]);

  // Execute a single background photo upload job seamlessly without blocking UI
  const executePhotoUploadJob = useCallback(
    async (job: PendingPhotoJob) => {
      try {
        const compressed = await compressAndUploadImage(job.file, ASSET_KIND.PHOTO, true);
        let liveAssetId: SnowflakeId | undefined;
        if (job.liveVideo) {
          liveAssetId = await uploadRawMediaBlob(
            job.liveVideo,
            ASSET_KIND.LIVE_MP4,
            job.liveVideo.type || 'video/mp4',
          );
        }
        const res = await api.createPhoto(job.spaceId, {
          asset_id: compressed.assetId,
          live_asset_id: liveAssetId,
          width: compressed.width,
          height: compressed.height,
          taken_at: compressed.takenAt,
          caption: job.caption,
          exif: { location: job.location.trim() || undefined },
        });

        recentlyUploadedRealIdsRef.current.add(res.photo.id);
        const remoteThumb =
          res.asset_urls?.[res.photo.asset_id]?.thumb_url ||
          res.asset_urls?.[res.photo.asset_id]?.full_url;
        await predecodeImageOffscreen(remoteThumb || undefined);

        mergeHydrated(res.users, res.asset_urls);
        setPhotos((prev) => {
          // Remove duplicate if SSE PHOTO_CREATED arrived a few ms earlier, then swap temp -> real!
          const withoutDup = prev.filter((p) => p.id !== res.photo.id);
          const idx = withoutDup.findIndex((p) => p.id === job.tempPhotoId);
          if (idx >= 0) {
            const next = [...withoutDup];
            next[idx] = res.photo;
            return next;
          }
          return [res.photo, ...withoutDup];
        });
        setSelectedPhoto((sel) => (sel?.id === job.tempPhotoId ? res.photo : sel));

        pendingPhotoJobsRef.current.delete(job.tempPhotoId);
        window.setTimeout(() => {
          URL.revokeObjectURL(job.preview);
          recentlyUploadedRealIdsRef.current.delete(res.photo.id);
        }, 5000);
      } catch {
        setPhotos((prev) =>
          prev.map((p) =>
            p.id === job.tempPhotoId ? { ...p, _transferState: 'failed' } : p,
          ),
        );
        toast('有一张照片传输未完成，可在画面上点击「重传」', 'error');
      }
    },
    [mergeHydrated],
  );

  // WeChat-style 0ms instant photo insertion + background async transfer
  const handleEnqueueUploads = useCallback(
    (
      items: QueuedPhotoUploadItem[],
      meta: { caption: string; location: string; liveVideo: File | null },
    ) => {
      if (!activeSpace || !currentUser || items.length === 0) return;

      const now = Date.now();
      const tempUrls: AssetUrlMap = {};
      const optimisticCards: PhotoCardDto[] = [];
      const jobs: PendingPhotoJob[] = [];

      items.forEach((item, idx) => {
        const suffix = `${now}_${idx}_${Math.random().toString(36).slice(2, 6)}`;
        const tempPhotoId = `temp_photo_${suffix}`;
        const tempAssetId = `temp_asset_${suffix}`;
        tempUrls[tempAssetId] = { full_url: item.preview, thumb_url: item.preview };

        optimisticCards.push({
          id: tempPhotoId,
          space_id: activeSpace.id,
          asset_id: tempAssetId,
          taken_at: now - idx,
          created_at: now - idx,
          post: {
            uploader_id: currentUser.id,
            width: item.width || 1600,
            height: item.height || 1200,
            caption: meta.caption.trim(),
            exif: { location: meta.location.trim() || null },
            likes: [],
            comments: [],
          },
          _transferState: 'sending',
        });

        const job: PendingPhotoJob = {
          tempPhotoId,
          tempAssetId,
          spaceId: activeSpace.id,
          file: item.file,
          preview: item.preview,
          caption: meta.caption,
          location: meta.location,
          liveVideo: items.length === 1 ? meta.liveVideo : null,
        };
        pendingPhotoJobsRef.current.set(tempPhotoId, job);
        jobs.push(job);
      });

      mergeHydrated(undefined, tempUrls);
      setPhotos((prev) => [...optimisticCards, ...prev]);

      // Run background uploads with bounded concurrency (2 parallel workers) so UI stays 60fps silky!
      void (async () => {
        const queue = [...jobs];
        const workers = Array.from({ length: Math.min(2, queue.length) }, async () => {
          while (queue.length > 0) {
            const nextJob = queue.shift();
            if (nextJob) await executePhotoUploadJob(nextJob);
          }
        });
        await Promise.all(workers);
      })();
    },
    [activeSpace, currentUser, executePhotoUploadJob, mergeHydrated],
  );

  const handleRetryUpload = useCallback(
    (tempPhotoId: SnowflakeId) => {
      const job = pendingPhotoJobsRef.current.get(tempPhotoId);
      if (!job) return;
      setPhotos((prev) =>
        prev.map((p) => (p.id === tempPhotoId ? { ...p, _transferState: 'sending' } : p)),
      );
      void executePhotoUploadJob(job);
    },
    [executePhotoUploadJob],
  );

  const handleDismissFailedUpload = useCallback((tempPhotoId: SnowflakeId) => {
    const job = pendingPhotoJobsRef.current.get(tempPhotoId);
    if (job) {
      URL.revokeObjectURL(job.preview);
      pendingPhotoJobsRef.current.delete(tempPhotoId);
    }
    setPhotos((prev) => prev.filter((p) => p.id !== tempPhotoId));
  }, []);

  // Full Space Real-time SSE Synchronization
  const { partnerInFridge, viewingPhotos, pendingNewCount, clearPendingNew } =
    useSpaceRealtime({
      spaceId: activeSpace?.id || null,
      selfUserId: currentUser?.id || '',
      onPhotoCreated: (resp) => {
        mergeHydrated(resp.users, resp.asset_urls);
        // If our own tab is actively swapping temp_photo -> real photo, executePhotoUploadJob handles it cleanly!
        if (recentlyUploadedRealIdsRef.current.has(resp.photo.id)) return;
        const hasMyInFlight = Array.from(pendingPhotoJobsRef.current.values()).some(
          (j) => j.spaceId === resp.photo.space_id && resp.photo.post.uploader_id === currentUser?.id,
        );
        if (hasMyInFlight) return;

        setPhotos((prev) => {
          if (prev.some((p) => p.id === resp.photo.id)) {
            return prev.map((p) => (p.id === resp.photo.id ? resp.photo : p));
          }
          const next = [resp.photo, ...prev];
          next.sort((a, b) => b.taken_at - a.taken_at || (b.id > a.id ? 1 : -1));
          return next;
        });
      },
      onPhotoMutated: (resp) => {
        mergeHydrated(resp.users, resp.asset_urls);
        setPhotos((prev) =>
          prev.map((p) => {
            if (p.id !== resp.photo.id) return p;
            const localPendingCmts = p.post.comments.filter((c) =>
              String(c.id).startsWith('temp_'),
            );
            return localPendingCmts.length === 0
              ? resp.photo
              : {
                  ...resp.photo,
                  post: {
                    ...resp.photo.post,
                    comments: [...resp.photo.post.comments, ...localPendingCmts],
                  },
                };
          }),
        );
        setSelectedPhoto((sel) => {
          if (!sel || sel.id !== resp.photo.id) return sel;
          const localPendingCmts = sel.post.comments.filter((c) =>
            String(c.id).startsWith('temp_'),
          );
          return localPendingCmts.length === 0
            ? resp.photo
            : {
                ...resp.photo,
                post: {
                  ...resp.photo.post,
                  comments: [...resp.photo.post.comments, ...localPendingCmts],
                },
              };
        });
      },
      onPhotoDeleted: (deletedId) => {
        setPhotos((prev) => prev.filter((p) => p.id !== deletedId));
        setSelectedPhoto((sel) => (sel?.id === deletedId ? null : sel));
      },
      onSpaceUpdated: () => {
        loadSpacesAndInvites(activeSpace?.id).catch(() => {});
        if (activeSpace?.is_shared) {
          api
            .getPet(activeSpace.id)
            .then((pRes) => {
              setPetResp(pRes);
              mergeHydrated(pRes.users, pRes.asset_urls);
            })
            .catch(() => {});
        }
      },
      onAgentToolboxUpdated: () => {
        setToolboxRefreshSeq((n) => n + 1);
      },
      onFridgeRemoteUpdate: () => {
        if (activeSpace) {
          void syncFridgePreservingOptimistic(activeSpace.id);
        }
      },
      onPetAgentEmote: (emote) => {
        if (emote.for_user_id && emote.for_user_id !== currentUser?.id) return;
        setPetAgentBubble({ pose: emote.pose, text: emote.bubble_text });
        if (bubbleTimerRef.current) window.clearTimeout(bubbleTimerRef.current);
        bubbleTimerRef.current = window.setTimeout(() => {
          setPetAgentBubble(null);
        }, 9000);
      },
      onPetRemoteUpdate: (pet, resonance) => {
        setPetResp((prev) => {
          if (!prev) return prev;
          if (pet.souvenirs.length > prev.pet.souvenirs.length && activeSpace) {
            api
              .getPet(activeSpace.id)
              .then((full) => {
                setPetResp(full);
                mergeHydrated(full.users, full.asset_urls);
              })
              .catch(() => {});
          }
          return {
            ...prev,
            pet,
            skills: {
              ...prev.skills,
              can_paw_pick_menu: pet.adopted && pet.current.behavior !== 'ADVENTURING',
            },
            sweet_resonance_triggered: Boolean(resonance),
          };
        });
      },
      onResyncNeeded: () => {
        if (activeSpace) {
          loadSpacesAndInvites(activeSpace.id).catch(() => {});
          loadSpaceData(activeSpace).catch(() => {});
        }
      },
    });

  const handleAuthenticated = async (session: AuthSessionResp) => {
    setCurrentUser(session.user);
    await loadSpacesAndInvites(session.default_personal_space_id);
  };

  const handleLogout = async () => {
    await api.logout().catch(() => {});
    setAccessToken(null);
    setCurrentUser(null);
    setSpaces([]);
  };

  const handleLoadMorePhotos = useCallback(async () => {
    if (!activeSpace || nextBeforeTakenAt == null || !nextBeforeId || loadingMorePhotos) return;
    setLoadingMorePhotos(true);
    try {
      const more = await api.listPhotos(
        activeSpace.id,
        nextBeforeTakenAt,
        nextBeforeId,
        36,
      );
      setPhotos((prev) => {
        const existingIds = new Set(prev.map((p) => p.id));
        return [...prev, ...more.photos.filter((p) => !existingIds.has(p.id))];
      });
      setNextBeforeTakenAt(more.next_before_taken_at || null);
      setNextBeforeId(more.next_before_id || null);
      mergeHydrated(more.users, more.asset_urls);
    } finally {
      setLoadingMorePhotos(false);
    }
  }, [activeSpace, nextBeforeTakenAt, nextBeforeId, loadingMorePhotos, mergeHydrated]);

  const selectedIdx = selectedPhoto ? photos.findIndex((p) => p.id === selectedPhoto.id) : -1;
  const navigatePhoto = useCallback(
    (dir: -1 | 1) => {
      if (selectedIdx < 0) return;
      const next = photos[selectedIdx + dir];
      if (next) setSelectedPhoto(next);
    },
    [photos, selectedIdx],
  );

  const handleOpenPhotoById = async (photoId: SnowflakeId) => {
    if (!activeSpace) return;
    const localHit = photos.find((p) => p.id === photoId);
    if (localHit) {
      setPetDrawerOpen(false);
      setSelectedPhoto(localHit);
      return;
    }
    try {
      const detail = await api.getPhotoDetail(activeSpace.id, photoId);
      mergeHydrated(detail.users, detail.asset_urls);
      setPetDrawerOpen(false);
      setSelectedPhoto(detail.photo);
    } catch {
      toast('这张旧照片已不在相册中', 'info');
    }
  };

  if (booting) {
    return (
      <div className="min-h-[100dvh] flex items-center justify-center bg-surface text-muted text-xs tracking-widest">
        KEEPDAYS
      </div>
    );
  }

  if (!currentUser || !activeSpace) {
    return (
      <AuthScreen
        isNight={ambient.isNight}
        themePreference={ambient.preference}
        onCycleTheme={ambient.cyclePreference}
        onAuthenticated={handleAuthenticated}
      />
    );
  }

  return (
    <div className="relative min-h-[100dvh] w-full bg-surface text-ink">
      {/* 1. Top Minimalist Floating Capsule */}
      <TopFloatingBar
        currentUser={currentUser}
        spaces={spaces}
        activeSpace={activeSpace}
        usersMap={usersMap}
        assetUrls={assetUrls}
        pendingInvites={pendingInvites}
        pendingNewCount={pendingNewCount}
        compactDensity={compactDensity}
        themePreference={ambient.preference}
        isNight={ambient.isNight}
        onCycleTheme={ambient.cyclePreference}
        onSelectSpace={(sid) => {
          setActiveSpaceId(sid);
          setSavedSpaceId(sid);
        }}
        onSpacesChanged={(selectId) => loadSpacesAndInvites(selectId)}
        onOpenMyProfile={() => setProfileDrawerUid(currentUser.id)}
        onOpenMemberProfile={(uid) => setProfileDrawerUid(uid)}
        onOpenUploadModal={() => setUploadModalOpen(true)}
        onToggleDensity={() => setCompactDensity((d) => !d)}
        onFlushPendingTimeline={() => {
          clearPendingNew();
          window.scrollTo({ top: 0, behavior: 'smooth' });
          loadSpaceData(activeSpace);
        }}
        onLogout={handleLogout}
      />

      {/* 2. Main Stage: Full-Screen Edge-to-Edge Photo Wall */}
      <PhotoWallCanvas
        spaceId={activeSpace.id}
        photos={photos}
        assetUrls={assetUrls}
        usersMap={usersMap}
        viewingPhotos={viewingPhotos}
        compactDensity={compactDensity}
        hasMore={Boolean(nextBeforeId)}
        loadingMore={loadingMorePhotos}
        initialLoading={wallLoading}
        uploadModalOpen={uploadModalOpen}
        onOpenUploadModal={() => setUploadModalOpen(true)}
        onCloseUploadModal={() => setUploadModalOpen(false)}
        onEnqueueUploads={handleEnqueueUploads}
        onRetryUpload={handleRetryUpload}
        onDismissFailedUpload={handleDismissFailedUpload}
        onSelectPhoto={(p) => setSelectedPhoto(p)}
        onLoadMore={handleLoadMorePhotos}
      />

      {/* 3. Shared Space Exclusive Vector Pet Roaming Along Photo Wall Ledges */}
      <RoamingPetLayer
        isSharedSpace={activeSpace.is_shared}
        fridgeOpen={fridgeStage !== 'collapsed'}
        compactDensity={compactDensity}
        petData={petResp}
        agentBubble={petAgentBubble}
        fridgeAnchorRef={fridgeAnchorRef}
        onOpenPetDrawer={() => {
          setPetAgentBubble(null);
          setPetDrawerOpen(true);
        }}
        onOpenOldMemoryPhoto={handleOpenPhotoById}
      />

      {/* 4. Bottom-Left Platform Toolbox (CC-Switch Hot-Swap Models + Markdown Memory + MCP) */}
      <PlatformToolbox
        spaceId={activeSpace.id}
        isSharedSpace={activeSpace.is_shared}
        hidden={fridgeStage !== 'collapsed'}
        open={toolboxOpen}
        initialTab={toolboxTab}
        externalRefreshSeq={toolboxRefreshSeq}
        onOpen={(tab) => {
          if (tab) setToolboxTab(tab);
          setToolboxOpen(true);
        }}
        onClose={() => setToolboxOpen(false)}
      />

      {/* 5. Bottom-Right Mini UI Fridge -> Matte Fridge Door -> Responsive Split View */}
      <FridgeSystem
        space={activeSpace}
        allSpaces={spaces}
        selfUserId={currentUser.id}
        stage={fridgeStage}
        isNightBeijing={ambient.isNight}
        partnerInFridge={partnerInFridge}
        canPetPickMenu={Boolean(petResp?.skills.can_paw_pick_menu)}
        bundle={fridgeBundle}
        fridgeAnchorRef={fridgeAnchorRef}
        onOpenFridge={handleOpenFridge}
        onChangeStage={handleChangeFridgeStage}
        onBundleChange={(next) => {
          setFridgeBundle(next);
          mergeHydrated(next.users, next.asset_urls);
        }}
      />

      {/* 6. WeChat-Moments Style Photo Lightbox + Optimistic Voice/Text Comments */}
      <Suspense fallback={null}>
        <AnimatePresence>
          {selectedPhoto && (
            <PhotoMomentsModal
              spaceId={activeSpace.id}
              selfUserId={currentUser.id}
              photo={selectedPhoto}
              photoIndex={selectedIdx >= 0 ? selectedIdx : 0}
              totalPhotos={photos.length}
              spaces={spaces}
              usersMap={usersMap}
              assetUrls={assetUrls}
              hasPrev={selectedIdx > 0}
              hasNext={selectedIdx >= 0 && selectedIdx < photos.length - 1}
              onNavigate={navigatePhoto}
              onClose={() => setSelectedPhoto(null)}
              onPhotoMutated={(updated, extraUrls) => {
                setSelectedPhoto(updated);
                setPhotos((prev) => prev.map((x) => (x.id === updated.id ? updated : x)));
                if (extraUrls) mergeHydrated(undefined, extraUrls);
              }}
              onPhotoDeleted={(pid) => {
                setSelectedPhoto(null);
                setPhotos((prev) => prev.filter((x) => x.id !== pid));
              }}
            />
          )}
        </AnimatePresence>

        {/* 7. Shared Pet Interactive Companion Drawer */}
        <AnimatePresence>
          {petDrawerOpen && petResp && activeSpace.is_shared && (
            <PetDrawer
              spaceId={activeSpace.id}
              selfUserId={currentUser.id}
              petResp={petResp}
              onClose={() => setPetDrawerOpen(false)}
              onPetChanged={(next) => {
                setPetResp(next);
                mergeHydrated(next.users, next.asset_urls);
              }}
              onOpenPhotoById={handleOpenPhotoById}
              onRefreshFridge={() => {
                void syncFridgePreservingOptimistic(activeSpace.id);
              }}
            />
          )}
        </AnimatePresence>

        {/* 8. Modular Profile Sheet */}
        <AnimatePresence>
          {profileDrawerUid && (
            <ProfileDrawer
              targetUserId={profileDrawerUid}
              selfUserId={currentUser.id}
              onClose={() => setProfileDrawerUid(null)}
              onSelfUpdated={(updated, urls) => {
                setCurrentUser(updated);
                mergeHydrated({ [updated.id]: updated }, urls);
              }}
            />
          )}
        </AnimatePresence>
      </Suspense>

      <Toaster />
    </div>
  );
};
