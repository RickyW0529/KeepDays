import type {
  A2uiSurfacePayload,
  A2uiUserAction,
  AgentMemoryInspectorResp,
  AgentSessionsBootstrapResp,
  AuthSessionResp,
  FetchLiveModelsResp,
  FridgeBundleResp,
  FullProfileResp,
  InvitationDto,
  McpServerConfigDto,
  PawPickMenuResp,
  PendingInvitationsResp,
  PetFullResp,
  PhotoExifMeta,
  PhotoWallResp,
  ProviderConfigDto,
  SessionMetaDto,
  SessionTurnRecord,
  SinglePhotoResp,
  SnowflakeId,
  SpaceDto,
  SpaceListResp,
  SpacePresenceResp,
  TimelinePageResp,
  UserProfileDoc,
} from '../types/api';

const TOKEN_STORAGE_KEY = 'kd_access_token';
const SPACE_STORAGE_KEY = 'kd_active_space_id';

let accessToken: string | null = localStorage.getItem(TOKEN_STORAGE_KEY);

export function getAccessToken(): string | null {
  return accessToken;
}

export function setAccessToken(token: string | null) {
  accessToken = token;
  if (token) {
    localStorage.setItem(TOKEN_STORAGE_KEY, token);
  } else {
    localStorage.removeItem(TOKEN_STORAGE_KEY);
  }
}

export function getSavedSpaceId(): SnowflakeId | null {
  return localStorage.getItem(SPACE_STORAGE_KEY);
}

export function setSavedSpaceId(spaceId: SnowflakeId) {
  localStorage.setItem(SPACE_STORAGE_KEY, spaceId);
}

let inflightRefresh: Promise<boolean> | null = null;

async function silentRefreshOnce(): Promise<boolean> {
  if (!inflightRefresh) {
    inflightRefresh = (async () => {
      try {
        const refreshRes = await fetch('/api/v1/auth/refresh', {
          method: 'POST',
          credentials: 'include',
        });
        if (!refreshRes.ok) return false;
        const session = (await refreshRes.json()) as AuthSessionResp;
        setAccessToken(session.access_token);
        return true;
      } catch {
        return false;
      } finally {
        inflightRefresh = null;
      }
    })();
  }
  return inflightRefresh;
}

async function request<T>(
  path: string,
  init: RequestInit = {},
  retryOn401 = true,
): Promise<T> {
  const headers = new Headers(init.headers);
  if (init.body && !headers.has('Content-Type')) {
    headers.set('Content-Type', 'application/json');
  }
  if (accessToken) {
    headers.set('Authorization', `Bearer ${accessToken}`);
  }

  const res = await fetch(`/api/v1${path}`, {
    ...init,
    headers,
    credentials: 'include',
  });

  if (res.status === 401 && retryOn401 && !path.startsWith('/auth/')) {
    const refreshed = await silentRefreshOnce();
    if (refreshed) {
      return request<T>(path, init, false);
    }
    setAccessToken(null);
    window.dispatchEvent(new CustomEvent('kd:unauthorized'));
  }

  if (!res.ok) {
    const errBody = await res.json().catch(() => ({ message: `请求失败 (${res.status})` }));
    throw new Error(errBody.message || `HTTP ${res.status}`);
  }

  return (await res.json()) as T;
}

export const api = {
  // Auth
  register: (username: string, password: string, nickname?: string) =>
    request<AuthSessionResp>('/auth/register', {
      method: 'POST',
      body: JSON.stringify({ username, password, nickname }),
    }),
  login: (username: string, password: string) =>
    request<AuthSessionResp>('/auth/login', {
      method: 'POST',
      body: JSON.stringify({ username, password }),
    }),
  refresh: () => request<AuthSessionResp>('/auth/refresh', { method: 'POST' }, false),
  logout: () => request<{ ok: boolean }>('/auth/logout', { method: 'POST' }, false),

  // LinkedIn-style Modular Profile
  getMyProfile: () => request<FullProfileResp>('/profile/me'),
  getUserProfile: (userId: SnowflakeId) => request<FullProfileResp>(`/profile/${userId}`),
  updateMyProfile: (doc: UserProfileDoc) =>
    request<FullProfileResp>('/profile/me', {
      method: 'PUT',
      body: JSON.stringify(doc),
    }),

  // Multi-Space Tenancy & Invitations
  listSpaces: () => request<SpaceListResp>('/spaces'),
  createSharedSpace: (name: string, aliasName?: string) =>
    request<SpaceDto>('/spaces', {
      method: 'POST',
      body: JSON.stringify({ name, alias_name: aliasName }),
    }),
  createInvitation: (spaceId: SnowflakeId, targetDoorplateNo?: string) =>
    request<InvitationDto>(`/spaces/${spaceId}/invitations`, {
      method: 'POST',
      body: JSON.stringify({ target_doorplate_no: targetDoorplateNo || null }),
    }),
  listPendingInvitations: () => request<PendingInvitationsResp>('/invitations/pending'),
  respondInvitation: (inviteCode: string, accept: boolean, aliasName?: string) =>
    request<{ accepted: boolean; space_id: SnowflakeId }>(
      `/invitations/${encodeURIComponent(inviteCode)}/respond`,
      {
        method: 'POST',
        body: JSON.stringify({ accept, alias_name: aliasName }),
      },
    ),

  // Silent Co-Presence
  updatePresence: (
    spaceId: SnowflakeId,
    focus: 'IDLE' | 'FRIDGE' | 'PHOTO',
    photoId?: SnowflakeId | null,
  ) =>
    request<SpacePresenceResp>(`/spaces/${spaceId}/presence`, {
      method: 'PUT',
      body: JSON.stringify({ focus, photo_id: photoId || null }),
    }),

  // Media Presigned Upload (Snowflake Dimension Domain 40)
  presignUpload: (kindId: number, mimeType: string, sizeBytes: number, hasThumb = false) =>
    request<{
      asset_id: SnowflakeId;
      kind_code: string;
      put_url: string;
      thumb_put_url?: string | null;
    }>('/media/presign-upload', {
      method: 'POST',
      body: JSON.stringify({
        kind_id: kindId,
        mime_type: mimeType,
        size_bytes: sizeBytes,
        has_thumb: hasThumb,
      }),
    }),
  confirmUpload: (assetId: SnowflakeId) =>
    request<{ asset_id: SnowflakeId; ready: boolean }>(`/media/${assetId}/confirm`, {
      method: 'POST',
    }),

  // Full-Screen Photo Wall & WeChat-Moments Voice/Text Comments
  listPhotos: (
    spaceId: SnowflakeId,
    beforeTakenAt?: number | null,
    beforeId?: string | null,
    limit = 36,
  ) => {
    const qs = new URLSearchParams({ limit: String(limit) });
    if (beforeTakenAt != null && beforeId != null) {
      qs.set('before_taken_at', String(beforeTakenAt));
      qs.set('before_id', beforeId);
    }
    return request<PhotoWallResp>(`/spaces/${spaceId}/photos?${qs.toString()}`);
  },
  createPhoto: (
    spaceId: SnowflakeId,
    payload: {
      asset_id: SnowflakeId;
      live_asset_id?: SnowflakeId | null;
      width: number;
      height: number;
      taken_at?: number;
      caption: string;
      exif?: PhotoExifMeta;
    },
  ) =>
    request<SinglePhotoResp>(`/spaces/${spaceId}/photos`, {
      method: 'POST',
      body: JSON.stringify(payload),
    }),
  getPhotoDetail: (spaceId: SnowflakeId, photoId: SnowflakeId) =>
    request<SinglePhotoResp>(`/spaces/${spaceId}/photos/${photoId}`),
  toggleLike: (spaceId: SnowflakeId, photoId: SnowflakeId) =>
    request<SinglePhotoResp>(`/spaces/${spaceId}/photos/${photoId}/like`, {
      method: 'POST',
    }),
  addComment: (
    spaceId: SnowflakeId,
    photoId: SnowflakeId,
    payload: {
      reply_to_comment_id?: SnowflakeId | null;
      text?: string;
      audio_asset_id?: SnowflakeId | null;
      duration_sec?: number;
      waveform?: number[];
    },
  ) =>
    request<SinglePhotoResp>(`/spaces/${spaceId}/photos/${photoId}/comments`, {
      method: 'POST',
      body: JSON.stringify(payload),
    }),
  deleteComment: (
    spaceId: SnowflakeId,
    photoId: SnowflakeId,
    commentId: SnowflakeId,
  ) =>
    request<SinglePhotoResp>(
      `/spaces/${spaceId}/photos/${photoId}/comments/${commentId}`,
      {
        method: 'DELETE',
      },
    ),
  projectPhoto: (
    spaceId: SnowflakeId,
    photoId: SnowflakeId,
    targetSpaceId: SnowflakeId,
    overrideCaption?: string,
  ) =>
    request<SinglePhotoResp>(`/spaces/${spaceId}/photos/${photoId}/project`, {
      method: 'POST',
      body: JSON.stringify({
        target_space_id: targetSpaceId,
        override_caption: overrideCaption || null,
      }),
    }),
  deletePhoto: (spaceId: SnowflakeId, photoId: SnowflakeId) =>
    request<{ deleted: boolean }>(`/spaces/${spaceId}/photos/${photoId}`, {
      method: 'DELETE',
    }),

  // UI Fridge, Family Menu & Frosted Time-Capsule Board
  getFridgeBundle: (spaceId: SnowflakeId) =>
    request<FridgeBundleResp>(`/spaces/${spaceId}/fridge/bundle`),
  updateMenu: (
    spaceId: SnowflakeId,
    expectedVersion: number,
    categories: string[],
    dishes: Array<{
      id?: SnowflakeId | null;
      name: string;
      category: string;
      cover_asset_id?: SnowflakeId | null;
      ingredients: string[];
      note: string;
    }>,
  ) =>
    request<FridgeBundleResp>(`/spaces/${spaceId}/fridge/menu`, {
      method: 'PUT',
      body: JSON.stringify({ expected_version: expectedVersion, categories, dishes }),
    }),
  pinMenuSlip: (
    spaceId: SnowflakeId,
    dishIds: SnowflakeId[],
    note: string,
    customDishes: string[] = [],
  ) =>
    request<FridgeBundleResp>(`/spaces/${spaceId}/fridge/slips`, {
      method: 'POST',
      body: JSON.stringify({ dish_ids: dishIds, custom_dishes: customDishes, note }),
    }),
  toggleSlipDone: (spaceId: SnowflakeId, slipId: SnowflakeId, done: boolean) =>
    request<FridgeBundleResp>(`/spaces/${spaceId}/fridge/slips/${slipId}`, {
      method: 'PATCH',
      body: JSON.stringify({ done }),
    }),
  convertSlipToGrocery: (spaceId: SnowflakeId, slipId: SnowflakeId) =>
    request<FridgeBundleResp>(`/spaces/${spaceId}/fridge/slips/${slipId}/to-grocery`, {
      method: 'POST',
    }),
  updateBoard: (
    spaceId: SnowflakeId,
    expectedVersion: number,
    notes: Array<{
      id?: SnowflakeId | null;
      uid?: SnowflakeId | null;
      text: string;
      color: string;
      pinned: boolean;
      unlock_at?: number | null;
      at?: number;
    }>,
    todos: Array<{
      id?: SnowflakeId | null;
      text: string;
      category?: string | null;
      done: boolean;
      done_by?: SnowflakeId | null;
      at?: number;
    }>,
  ) =>
    request<FridgeBundleResp>(`/spaces/${spaceId}/fridge/board`, {
      method: 'PUT',
      body: JSON.stringify({ expected_version: expectedVersion, notes, todos }),
    }),
  projectDishToSpace: (
    spaceId: SnowflakeId,
    targetSpaceId: SnowflakeId,
    dishId: SnowflakeId,
  ) =>
    request<{ projected: boolean }>(
      `/spaces/${spaceId}/fridge/menu/project/${targetSpaceId}`,
      {
        method: 'POST',
        body: JSON.stringify({ dish_id: dishId }),
      },
    ),

  // Shared Pet Engine
  getPet: (spaceId: SnowflakeId) => request<PetFullResp>(`/spaces/${spaceId}/pet`),
  adoptPet: (spaceId: SnowflakeId, name: string, species: string) =>
    request<PetFullResp>(`/spaces/${spaceId}/pet/adopt`, {
      method: 'POST',
      body: JSON.stringify({ name, species }),
    }),
  interactPet: (spaceId: SnowflakeId, action: string, itemId?: string) =>
    request<PetFullResp>(`/spaces/${spaceId}/pet/interact`, {
      method: 'POST',
      body: JSON.stringify({ action, item_id: itemId || null }),
    }),
  buyPetItem: (spaceId: SnowflakeId, itemId: string, quantity = 1) =>
    request<PetFullResp>(`/spaces/${spaceId}/pet/shop/buy`, {
      method: 'POST',
      body: JSON.stringify({ item_id: itemId, quantity }),
    }),
  equipPetDecor: (spaceId: SnowflakeId, bedStyle?: string, accessory?: string | null) =>
    request<PetFullResp>(`/spaces/${spaceId}/pet/equip`, {
      method: 'POST',
      body: JSON.stringify({ bed_style: bedStyle, accessory }),
    }),
  startPetAdventure: (spaceId: SnowflakeId) =>
    request<PetFullResp>(`/spaces/${spaceId}/pet/adventure`, {
      method: 'POST',
    }),
  pawPickMenu: (spaceId: SnowflakeId) =>
    request<PawPickMenuResp>(`/spaces/${spaceId}/pet/paw-pick-menu`, {
      method: 'POST',
    }),

  // Twitter Fan-out Timeline
  getTimeline: (spaceId: SnowflakeId, beforeId?: string | null, limit = 20) => {
    const qs = new URLSearchParams({ limit: String(limit) });
    if (beforeId) qs.set('before_id', beforeId);
    return request<TimelinePageResp>(`/spaces/${spaceId}/timeline?${qs.toString()}`);
  },

  // Pet Agent Kernel (S3 Sessions + 3-Tier Memory + Multi-Provider + MCP + A2UI)
  listAgentSessions: (spaceId: SnowflakeId) =>
    request<AgentSessionsBootstrapResp>(`/spaces/${spaceId}/pet/agent/sessions`),
  createAgentSession: (spaceId: SnowflakeId, title?: string) =>
    request<SessionMetaDto>(`/spaces/${spaceId}/pet/agent/sessions`, {
      method: 'POST',
      body: JSON.stringify({ title }),
    }),
  getAgentSessionDetail: (spaceId: SnowflakeId, sessionId: string) =>
    request<{ session: SessionMetaDto; turns: SessionTurnRecord[] }>(
      `/spaces/${spaceId}/pet/agent/sessions/${encodeURIComponent(sessionId)}`,
    ),
  getAgentMemory: (spaceId: SnowflakeId) =>
    request<AgentMemoryInspectorResp>(`/spaces/${spaceId}/pet/agent/memory`),
  updateAgentMemoryMarkdown: (
    spaceId: SnowflakeId,
    scope: 'shared' | 'private',
    markdown: string,
  ) =>
    request<AgentMemoryInspectorResp>(`/spaces/${spaceId}/pet/agent/memory/markdown`, {
      method: 'PUT',
      body: JSON.stringify({ scope, markdown }),
    }),
  revertAgentLtmCommit: (spaceId: SnowflakeId, revId: string) =>
    request<{ reverted_commit: unknown }>(
      `/spaces/${spaceId}/pet/agent/memory/revert/${encodeURIComponent(revId)}`,
      { method: 'POST' },
    ),
  getAgentProviders: (spaceId: SnowflakeId) =>
    request<{ providers: ProviderConfigDto[] }>(`/spaces/${spaceId}/pet/agent/providers`),
  upsertAgentProvider: (spaceId: SnowflakeId, cfg: Partial<ProviderConfigDto>) =>
    request<{ providers: ProviderConfigDto[] }>(`/spaces/${spaceId}/pet/agent/providers`, {
      method: 'PUT',
      body: JSON.stringify(cfg),
    }),
  activateAgentProvider: (spaceId: SnowflakeId, providerId: string) =>
    request<{ providers: ProviderConfigDto[] }>(
      `/spaces/${spaceId}/pet/agent/providers/${encodeURIComponent(providerId)}/activate`,
      { method: 'POST' },
    ),
  probeAgentProvider: (spaceId: SnowflakeId, providerId: string) =>
    request<{ providers: ProviderConfigDto[] }>(
      `/spaces/${spaceId}/pet/agent/providers/${encodeURIComponent(providerId)}/probe`,
      { method: 'POST' },
    ),
  fetchAgentVendorModels: (
    spaceId: SnowflakeId,
    payload: { vendor: string; provider_id?: string; api_key?: string },
  ) =>
    request<FetchLiveModelsResp>(`/spaces/${spaceId}/pet/agent/providers/models`, {
      method: 'POST',
      body: JSON.stringify(payload),
    }),
  deleteAgentProvider: (spaceId: SnowflakeId, providerId: string) =>
    request<{ providers: ProviderConfigDto[] }>(
      `/spaces/${spaceId}/pet/agent/providers/${encodeURIComponent(providerId)}`,
      { method: 'DELETE' },
    ),
  getAgentMcpServers: (spaceId: SnowflakeId) =>
    request<{ mcp_servers: McpServerConfigDto[] }>(`/spaces/${spaceId}/pet/agent/mcp`),
  upsertAgentMcpServer: (spaceId: SnowflakeId, cfg: McpServerConfigDto) =>
    request<{ mcp_servers: McpServerConfigDto[] }>(`/spaces/${spaceId}/pet/agent/mcp`, {
      method: 'PUT',
      body: JSON.stringify(cfg),
    }),
  probeAgentMcpServer: (spaceId: SnowflakeId, mcpId: string) =>
    request<{ mcp_servers: McpServerConfigDto[] }>(
      `/spaces/${spaceId}/pet/agent/mcp/${encodeURIComponent(mcpId)}/probe`,
      { method: 'POST' },
    ),
  deleteAgentMcpServer: (spaceId: SnowflakeId, mcpId: string) =>
    request<{ mcp_servers: McpServerConfigDto[] }>(
      `/spaces/${spaceId}/pet/agent/mcp/${encodeURIComponent(mcpId)}`,
      { method: 'DELETE' },
    ),
};

export interface AgentStreamCallbacks {
  onSessionMeta?: (meta: { session_id: string; pet_pose?: string }) => void;
  onTextDelta?: (delta: string) => void;
  onToolStart?: (ev: { call_id: string; tool_name: string }) => void;
  onToolEnd?: (ev: { call_id: string; tool_name: string; ok: boolean; summary: string }) => void;
  onSurface?: (surface: A2uiSurfacePayload) => void;
  onTurnDone?: (done: {
    session_id: string;
    turn_id: string;
    title: string;
    pet_pose?: string;
  }) => void;
  onError?: (message: string) => void;
}

export async function streamAgentTurn(
  spaceId: SnowflakeId,
  body: { session_id?: string | null; text?: string; ui_action?: A2uiUserAction },
  cb: AgentStreamCallbacks,
  retryOn401 = true,
): Promise<void> {
  const headers: Record<string, string> = {
    'Content-Type': 'application/json',
    Accept: 'text/event-stream',
  };
  if (accessToken) {
    headers.Authorization = `Bearer ${accessToken}`;
  }

  const res = await fetch(`/api/v1/spaces/${spaceId}/pet/agent/chat/stream`, {
    method: 'POST',
    headers,
    credentials: 'include',
    body: JSON.stringify(body),
  });

  if (res.status === 401 && retryOn401) {
    const ok = await silentRefreshOnce();
    if (ok) {
      return streamAgentTurn(spaceId, body, cb, false);
    }
  }
  if (!res.ok || !res.body) {
    const err = await res.json().catch(() => ({ message: `请求失败 (${res.status})` }));
    throw new Error(err.message || `HTTP ${res.status}`);
  }

  const reader = res.body.getReader();
  const decoder = new TextDecoder();
  let buf = '';

  const dispatchBlock = (block: string) => {
    let eventName = 'message';
    const dataLines: string[] = [];
    for (const rawLine of block.split('\n')) {
      const line = rawLine.replace(/\r$/, '');
      if (line.startsWith('event:')) {
        eventName = line.slice(6).trim();
      } else if (line.startsWith('data:')) {
        dataLines.push(line.slice(5).trimStart());
      }
    }
    if (dataLines.length === 0) return;
    const rawData = dataLines.join('\n');
    try {
      const parsed = JSON.parse(rawData);
      switch (eventName) {
        case 'session_meta':
          cb.onSessionMeta?.(parsed);
          break;
        case 'text_delta':
          cb.onTextDelta?.(parsed.delta || '');
          break;
        case 'tool_start':
          cb.onToolStart?.(parsed);
          break;
        case 'tool_end':
          cb.onToolEnd?.(parsed);
          break;
        case 'a2ui_surface':
          if (parsed.surface) cb.onSurface?.(parsed.surface);
          break;
        case 'turn_done':
          cb.onTurnDone?.(parsed);
          break;
        case 'error':
          cb.onError?.(parsed.message || '执行异常');
          break;
      }
    } catch {
      // ignore keepalive comments
    }
  };

  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    buf += decoder.decode(value, { stream: true });
    let sepIdx: number;
    while ((sepIdx = buf.indexOf('\n\n')) !== -1) {
      const block = buf.slice(0, sepIdx);
      buf = buf.slice(sepIdx + 2);
      dispatchBlock(block);
    }
  }
  if (buf.trim()) {
    dispatchBlock(buf);
  }
}
