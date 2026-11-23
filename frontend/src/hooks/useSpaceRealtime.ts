import { useCallback, useEffect, useRef, useState } from 'react';
import { getAccessToken, silentRefreshOnce } from '../lib/apiClient';
import { predecodeImageOffscreen } from '../lib/mediaProcessor';
import type {
  PetAgentEmotePayload,
  PetStateDoc,
  PresenceFocus,
  RealtimeSseEnvelope,
  SinglePhotoResp,
  SnowflakeId,
  SpacePresenceResp,
  TimelineItemDto,
} from '../types/api';

interface UseSpaceRealtimeOpts {
  spaceId: SnowflakeId | null;
  selfUserId: SnowflakeId;
  onFridgeRemoteUpdate: () => void;
  onPetRemoteUpdate: (pet: PetStateDoc, resonance?: boolean) => void;
  onPetAgentEmote?: (emote: PetAgentEmotePayload) => void;
  onPhotoCreated?: (resp: SinglePhotoResp) => void;
  onPhotoMutated?: (resp: SinglePhotoResp) => void;
  onPhotoDeleted?: (photoId: SnowflakeId) => void;
  onSpaceUpdated?: () => void;
  onAgentToolboxUpdated?: () => void;
  onResyncNeeded?: () => void;
}

export function useSpaceRealtime({
  spaceId,
  selfUserId,
  onFridgeRemoteUpdate,
  onPetRemoteUpdate,
  onPetAgentEmote,
  onPhotoCreated,
  onPhotoMutated,
  onPhotoDeleted,
  onSpaceUpdated,
  onAgentToolboxUpdated,
  onResyncNeeded,
}: UseSpaceRealtimeOpts) {
  const [partnerInFridge, setPartnerInFridge] = useState(false);
  const [viewingPhotos, setViewingPhotos] = useState<Record<SnowflakeId, SnowflakeId[]>>({});
  const [pendingNewCount, setPendingNewCount] = useState(0);

  const cbRef = useRef({
    onFridgeRemoteUpdate,
    onPetRemoteUpdate,
    onPetAgentEmote,
    onPhotoCreated,
    onPhotoMutated,
    onPhotoDeleted,
    onSpaceUpdated,
    onAgentToolboxUpdated,
    onResyncNeeded,
  });
  cbRef.current = {
    onFridgeRemoteUpdate,
    onPetRemoteUpdate,
    onPetAgentEmote,
    onPhotoCreated,
    onPhotoMutated,
    onPhotoDeleted,
    onSpaceUpdated,
    onAgentToolboxUpdated,
    onResyncNeeded,
  };

  const applyPresenceList = useCallback(
    (list: PresenceFocus[]) => {
      let inFridge = false;
      const map: Record<SnowflakeId, SnowflakeId[]> = {};
      for (const p of list) {
        if (p.user_id !== selfUserId) {
          if (p.focus === 'FRIDGE') inFridge = true;
          if (p.focus === 'PHOTO' && p.photo_id) {
            (map[p.photo_id] ||= []).push(p.user_id);
          }
        }
      }
      setPartnerInFridge(inFridge);
      setViewingPhotos(map);
    },
    [selfUserId],
  );

  useEffect(() => {
    setPartnerInFridge(false);
    setViewingPhotos({});
    setPendingNewCount(0);

    if (!spaceId) return;

    let disposed = false;
    let es: EventSource | null = null;
    let reconnectTimer: number | null = null;
    let retryAttempt = 0;

    const connect = (token: string) => {
      if (disposed) return;
      es?.close();
      es = new EventSource(
        `/api/v1/spaces/${spaceId}/events?token=${encodeURIComponent(token)}`,
      );

      es.addEventListener('CONNECTED', (ev) => {
        retryAttempt = 0;
        try {
          const env = JSON.parse((ev as MessageEvent).data) as RealtimeSseEnvelope<{
            presence: SpacePresenceResp;
          }>;
          if (env.payload?.presence) {
            setPartnerInFridge(env.payload.presence.partner_in_fridge);
            setViewingPhotos(env.payload.presence.viewing_photos || {});
          }
        } catch {
          // ignore
        }
      });

      es.addEventListener('PRESENCE_SYNC', (ev) => {
        try {
          const env = JSON.parse((ev as MessageEvent).data) as RealtimeSseEnvelope<PresenceFocus[]>;
          if (Array.isArray(env.payload)) {
            applyPresenceList(env.payload);
          }
        } catch {
          // ignore
        }
      });

      // Live real-time photo wall insertion (pre-decodes image offscreen first for 0-jank!)
      es.addEventListener('PHOTO_CREATED', async (ev) => {
        try {
          const env = JSON.parse(
            (ev as MessageEvent).data,
          ) as RealtimeSseEnvelope<SinglePhotoResp>;
          const resp = env.payload;
          if (!resp?.photo) return;
          const aid = resp.photo.asset_id;
          const url = resp.asset_urls?.[aid]?.thumb_url || resp.asset_urls?.[aid]?.full_url;
          await predecodeImageOffscreen(url || undefined);
          if (!disposed) {
            cbRef.current.onPhotoCreated?.(resp);
          }
        } catch {
          // ignore
        }
      });

      // Live real-time comments, voice notes, comment deletes, and likes
      es.addEventListener('PHOTO_MUTATED', (ev) => {
        try {
          const env = JSON.parse(
            (ev as MessageEvent).data,
          ) as RealtimeSseEnvelope<SinglePhotoResp>;
          if (env.payload?.photo) {
            cbRef.current.onPhotoMutated?.(env.payload);
          }
        } catch {
          // ignore
        }
      });

      // Live real-time photo removal
      es.addEventListener('PHOTO_DELETED', (ev) => {
        try {
          const env = JSON.parse((ev as MessageEvent).data) as RealtimeSseEnvelope<{
            photo_id: SnowflakeId;
          }>;
          if (env.payload?.photo_id) {
            cbRef.current.onPhotoDeleted?.(env.payload.photo_id);
          }
        } catch {
          // ignore
        }
      });

      es.addEventListener('SPACE_UPDATED', () => {
        cbRef.current.onSpaceUpdated?.();
      });

      es.addEventListener('TIMELINE_NEW', (ev) => {
        try {
          const env = JSON.parse((ev as MessageEvent).data) as RealtimeSseEnvelope<TimelineItemDto>;
          const item = env.payload;
          if (!item || item.actor_id === selfUserId) return;
          // Photo events are already synced directly via PHOTO_CREATED / PHOTO_MUTATED!
          if (
            item.event_code === 'PHOTO_POSTED' ||
            item.event_code === 'PHOTO_COMMENTED' ||
            item.event_code === 'PHOTO_LIKED'
          ) {
            return;
          }
        } catch {
          // ignore
        }
      });

      es.addEventListener('FRIDGE_UPDATED', () => {
        cbRef.current.onFridgeRemoteUpdate();
      });

      es.addEventListener('BOARD_UPDATED', () => {
        cbRef.current.onFridgeRemoteUpdate();
      });

      es.addEventListener('AGENT_CONFIG_UPDATED', () => {
        cbRef.current.onAgentToolboxUpdated?.();
      });

      es.addEventListener('AGENT_MEMORY_UPDATED', () => {
        cbRef.current.onAgentToolboxUpdated?.();
      });

      es.addEventListener('PET_AGENT_EMOTE', (ev) => {
        try {
          const env = JSON.parse(
            (ev as MessageEvent).data,
          ) as RealtimeSseEnvelope<PetAgentEmotePayload>;
          if (env.payload) {
            cbRef.current.onPetAgentEmote?.(env.payload);
          }
        } catch {
          // ignore
        }
      });

      es.addEventListener('PET_UPDATED', (ev) => {
        try {
          const env = JSON.parse((ev as MessageEvent).data) as RealtimeSseEnvelope<
            PetStateDoc | { pet: PetStateDoc; sweet_resonance?: boolean }
          >;
          if ('adopted' in (env.payload as PetStateDoc)) {
            cbRef.current.onPetRemoteUpdate(env.payload as PetStateDoc, false);
          } else if (env.payload && 'pet' in env.payload) {
            cbRef.current.onPetRemoteUpdate(env.payload.pet, env.payload.sweet_resonance);
          }
        } catch {
          // ignore
        }
      });

      es.addEventListener('RESYNC', () => {
        cbRef.current.onResyncNeeded?.();
      });

      // Auto-recover across 15-min JWT expiry, mobile tab wakeups, and Wi-Fi switches!
      es.onerror = () => {
        es?.close();
        if (disposed) return;
        const delay = Math.min(20_000, 1200 * Math.pow(2, retryAttempt));
        retryAttempt = Math.min(retryAttempt + 1, 5);
        reconnectTimer = window.setTimeout(async () => {
          if (disposed) return;
          const ok = await silentRefreshOnce();
          const latest = getAccessToken();
          if (latest) {
            connect(latest);
            if (ok) {
              cbRef.current.onResyncNeeded?.();
            }
          }
        }, delay);
      };
    };

    const initialToken = getAccessToken();
    if (initialToken) {
      connect(initialToken);
    }

    // Resync immediately when user returns from background tab or regains network
    const onVisOrOnline = () => {
      if (document.visibilityState === 'visible') {
        cbRef.current.onResyncNeeded?.();
        if (!es || es.readyState === EventSource.CLOSED) {
          const tok = getAccessToken();
          if (tok) connect(tok);
        }
      }
    };
    document.addEventListener('visibilitychange', onVisOrOnline);
    window.addEventListener('online', onVisOrOnline);

    return () => {
      disposed = true;
      document.removeEventListener('visibilitychange', onVisOrOnline);
      window.removeEventListener('online', onVisOrOnline);
      if (reconnectTimer) window.clearTimeout(reconnectTimer);
      es?.close();
    };
  }, [spaceId, selfUserId, applyPresenceList]);

  const clearPendingNew = useCallback(() => setPendingNewCount(0), []);

  return {
    partnerInFridge,
    viewingPhotos,
    pendingNewCount,
    clearPendingNew,
  };
}
