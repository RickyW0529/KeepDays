/**
 * KeepDays Frontend <-> Rust Backend Strict TypeScript Contract
 *
 * Note on 64-bit Snowflake IDs:
 * All backend `i64` Snowflake IDs are serialized as JSON `string` (`SnowflakeId`)
 * to prevent JavaScript IEEE-754 53-bit precision truncation.
 */

export type SnowflakeId = string;

export type AmbientPhase = 'DAYLIGHT' | 'DUSK' | 'NIGHT';

export interface AmbientStatus {
  timezone: 'Asia/Shanghai';
  beijing_iso: string;
  date_label: string;
  hour: number;
  minute: number;
  phase: AmbientPhase;
  fridge_nightlight_on: boolean;
  canvas_bg_hex: string;
}

export interface CachedAssetUrls {
  full_url: string;
  thumb_url?: string | null;
}

export type AssetUrlMap = Record<SnowflakeId, CachedAssetUrls>;

// ============================================================================
// 1. LinkedIn-Style Modular User Profile
// ============================================================================

export interface ProfileHighlight {
  year: string;
  title: string;
  photo_asset_id?: SnowflakeId | null;
}

export interface ProfileSection {
  id: string;
  title: string;
  items: string[];
  highlights: ProfileHighlight[];
}

export interface UserProfileDoc {
  nickname: string;
  signature: string;
  avatar_asset_id?: SnowflakeId | null;
  banner_asset_id?: SnowflakeId | null;
  sections: ProfileSection[];
}

export interface UserSummaryDto {
  id: SnowflakeId;
  username: string;
  doorplate_no: string; // e.g. "89757"
  profile: UserProfileDoc;
}

export type UserMap = Record<SnowflakeId, UserSummaryDto>;

export interface AuthSessionResp {
  access_token: string;
  expires_in_secs: number;
  user: UserSummaryDto;
  default_personal_space_id: SnowflakeId;
}

// ============================================================================
// 2. Multi-Space Tenancy & Co-Presence ("冰箱透暖光 & 照片微光框")
// ============================================================================

export interface SpaceMemberDto {
  user_id: SnowflakeId;
  role_id: number; // 2001=OWNER, 2002=MEMBER
  alias_name?: string | null;
  joined_at: number;
}

export interface SpaceDto {
  id: SnowflakeId;
  name: string;
  space_type_id: number; // 1001=PERSONAL, 1002=SHARED
  is_shared: boolean;
  owner_id: SnowflakeId;
  created_at: number;
  members: SpaceMemberDto[];
}

export interface PresenceFocus {
  user_id: SnowflakeId;
  focus: 'IDLE' | 'FRIDGE' | 'PHOTO';
  photo_id?: SnowflakeId | null;
  updated_at_ms: number;
}

export interface SpacePresenceResp {
  active_members: PresenceFocus[];
  /** Lights up warm yellow light slit on the bottom-right mini fridge! */
  partner_in_fridge: boolean;
  /** photo_id -> user_ids currently viewing that photo */
  viewing_photos: Record<SnowflakeId, SnowflakeId[]>;
}

// ============================================================================
// 3. Full-Screen Photo Wall & WeChat-Moments Voice/Text Comments
// ============================================================================

export interface PhotoExifMeta {
  location?: string | null;
  lat?: number | null;
  lng?: number | null;
  device?: string | null;
  lens?: string | null;
}

export type OptimisticTransferState = 'sending' | 'failed';

export interface PhotoComment {
  id: SnowflakeId;
  uid: SnowflakeId;
  reply_to_comment_id?: SnowflakeId | null;
  text?: string | null;
  audio_asset_id?: SnowflakeId | null;
  duration_sec?: number | null;
  /** 16~24 normalized integers (0..100) for instant minimalist声波 rendering */
  waveform: number[];
  at: number;
  _transferState?: OptimisticTransferState;
  _localAudioBlob?: Blob;
}

export interface PhotoPostDoc {
  uploader_id: SnowflakeId;
  live_asset_id?: SnowflakeId | null;
  width: number;
  height: number;
  caption: string;
  exif: PhotoExifMeta;
  likes: SnowflakeId[];
  comments: PhotoComment[];
}

export interface PhotoCardDto {
  id: SnowflakeId;
  space_id: SnowflakeId;
  asset_id: SnowflakeId;
  taken_at: number;
  created_at: number;
  post: PhotoPostDoc;
  _transferState?: OptimisticTransferState;
}

// ============================================================================
// 4. UI Fridge (Recent 5 Slips + Menu + Frosted Time-Capsule Board)
// ============================================================================

export interface FridgeSlip {
  id: SnowflakeId;
  date_label: string;
  by_uid: SnowflakeId;
  dish_ids: SnowflakeId[];
  dishes: string[];
  note: string;
  grocery_synced: boolean;
  done: boolean;
  at: number;
}

export interface MenuDish {
  id: SnowflakeId;
  name: string;
  category: string;
  cover_asset_id?: SnowflakeId | null;
  ingredients: string[];
  note: string;
}

export interface BoardNote {
  id: SnowflakeId;
  uid: SnowflakeId;
  text: string; // Redacted to "" by backend when `locked === true`!
  color: 'cotton_white' | 'oat' | 'sage' | 'blush' | string;
  pinned: boolean;
  unlock_at?: number | null;
  locked: boolean;
  at: number;
  _transferState?: OptimisticTransferState;
}

export interface BoardTodo {
  id: SnowflakeId;
  text: string;
  category?: string | null;
  done: boolean;
  done_by?: SnowflakeId | null;
  done_at?: number | null;
  at: number;
  _transferState?: OptimisticTransferState;
}

export interface VersionedModule<T> {
  version: number;
  data: T;
  updated_at: number;
}

export interface FridgeBundleResp {
  fridge: VersionedModule<{ recent_slips: FridgeSlip[] }>;
  menu: VersionedModule<{ categories: string[]; dishes: MenuDish[] }>;
  board: VersionedModule<{ notes: BoardNote[]; todos: BoardTodo[] }>;
  users: UserMap;
  asset_urls: AssetUrlMap;
}

// ============================================================================
// 5. Shared Space Vector Pet Engine (Wall Roaming + Co-Op + Adventure)
// ============================================================================

export type PetBehavior =
  | 'SLEEPING_FRIDGE'
  | 'SUNBATHING_FRIDGE'
  | 'ROAMING_WALL'
  | 'PEEKING_PHOTO'
  | 'CARRYING_OLD_PHOTO'
  | 'WATCHING_MENU'
  | 'PLAYING_BOX'
  | 'ADVENTURING';

export interface PetStateDoc {
  adopted: boolean;
  name: string;
  species: 'BLACK_CAT' | 'SHIBA' | 'OTTER' | 'BUNNY' | string;
  stage: 'BABY' | 'GROWING' | 'MATURE' | 'GUARDIAN';
  level: number;
  exp: number;
  paw_coins: number;
  stats: { satiety: number; energy: number; mood: number; bond: number };
  traits: {
    clingy_vs_independent: number;
    foodie_vs_athletic: number;
    curiosity: number;
  };
  current: {
    behavior: PetBehavior;
    focus_photo_id?: SnowflakeId | null;
    carrying_old_photo_id?: SnowflakeId | null;
    carrying_old_asset_id?: SnowflakeId | null;
    adventure_started_at?: number | null;
    adventure_return_at?: number | null;
    equipped_accessory?: string | null;
    bed_style: string;
  };
  inventory: Record<string, number>;
  unlocked_decor: string[];
  coop_quest?: {
    quest_id: string;
    title: string;
    step1_by: SnowflakeId;
    step1_at: number;
    expires_at: number;
  } | null;
  souvenirs: Array<{
    id: SnowflakeId;
    kind: 'MEMORY_POSTCARD' | 'NATURE_GIFT';
    title: string;
    ref_photo_id?: SnowflakeId | null;
    ref_asset_id?: SnowflakeId | null;
    diary: string;
    at: number;
  }>;
  logs: Array<{
    uid: SnowflakeId;
    action: string;
    text: string;
    resonance: boolean;
    at: number;
  }>;
  adopted_at?: number | null;
  last_tick_at: number;
}

// ============================================================================
// 6. Twitter-Style Fan-out Timeline & SSE Real-Time Events
// ============================================================================

export interface TimelineItemDto {
  id: SnowflakeId;
  space_id: SnowflakeId;
  actor_id: SnowflakeId;
  event_type_id: number;
  event_code: string;
  payload: Record<string, unknown>;
  created_at: number;
}

export interface TimelinePageResp {
  items: TimelineItemDto[];
  next_cursor?: string | null;
  users: UserMap;
  asset_urls: AssetUrlMap;
}

export interface SpaceListResp {
  spaces: SpaceDto[];
  users: UserMap;
  asset_urls: AssetUrlMap;
}

export interface InvitationDto {
  id: SnowflakeId;
  space_id: SnowflakeId;
  space_name: string;
  inviter_id: SnowflakeId;
  invitee_id?: SnowflakeId | null;
  invite_code: string;
  expires_at: number;
  created_at: number;
}

export interface PendingInvitationsResp {
  invitations: InvitationDto[];
  users: UserMap;
  asset_urls: AssetUrlMap;
}

export interface PhotoWallResp {
  photos: PhotoCardDto[];
  next_before_taken_at?: number | null;
  next_before_id?: string | null;
  users: UserMap;
  asset_urls: AssetUrlMap;
}

export interface SinglePhotoResp {
  photo: PhotoCardDto;
  users: UserMap;
  asset_urls: AssetUrlMap;
}

export interface ShopItemDef {
  item_id: string;
  name: string;
  category: 'FOOD' | 'DUO_FOOD' | 'BED' | 'ACCESSORY';
  price_coins: number;
  satiety_gain: number;
  mood_gain: number;
  energy_gain: number;
  desc: string;
}

export interface PetFullResp {
  pet: PetStateDoc;
  skills: {
    can_paw_pick_menu: boolean;
    has_unread_board_letter: boolean;
  };
  shop_catalog: ShopItemDef[];
  sweet_resonance_triggered: boolean;
  users: UserMap;
  asset_urls: AssetUrlMap;
}

export interface PawPickMenuResp {
  pet_name: string;
  pet_commentary: string;
  picked_dish_ids: SnowflakeId[];
  picked_dishes: MenuDish[];
}

export interface FullProfileResp {
  user: UserSummaryDto;
  asset_urls: AssetUrlMap;
}

export interface RealtimeSseEnvelope<T = unknown> {
  space_id: SnowflakeId;
  kind:
    | 'CONNECTED'
    | 'TIMELINE_NEW'
    | 'PHOTO_CREATED'
    | 'PHOTO_MUTATED'
    | 'PHOTO_DELETED'
    | 'SPACE_UPDATED'
    | 'PRESENCE_SYNC'
    | 'FRIDGE_UPDATED'
    | 'BOARD_UPDATED'
    | 'PET_UPDATED'
    | 'PET_AGENT_EMOTE'
    | 'AGENT_CONFIG_UPDATED'
    | 'AGENT_MEMORY_UPDATED';
  payload: T;
  ts_ms: number;
}

// ============================================================================
// 6. Pet Agent Kernel & Google A2UI Generative UI Surfaces
// ============================================================================

export type A2uiTriggerPolicy =
  | { mode: 'immediate'; action?: string }
  | { mode: 'debounce'; delay_ms?: number; action?: string }
  | { mode: 'on_all_checked'; path?: string; action?: string }
  | { mode: 'submit'; submit_label?: string; action?: string };

export interface A2uiComponentNode {
  id: string;
  component: string;
  children?: string[];
  bind_path?: string | null;
  props?: Record<string, unknown>;
}

export interface A2uiSurfacePayload {
  surface_id: string;
  title?: string | null;
  root_id: string;
  components: A2uiComponentNode[];
  data_model: Record<string, unknown>;
  trigger: A2uiTriggerPolicy;
  submitted?: boolean;
  submitted_summary?: string | null;
}

export interface A2uiUserAction {
  surface_id: string;
  action_name: string;
  component_id?: string | null;
  value?: unknown;
  data_model: Record<string, unknown>;
  summary_label?: string | null;
}

export interface ToolTraceSummary {
  call_id: string;
  tool_name: string;
  ok: boolean;
  summary: string;
}

export interface SessionTurnRecord {
  turn_id: string;
  role: 'user' | 'assistant';
  text: string;
  ui_action_summary?: string | null;
  tool_traces: ToolTraceSummary[];
  surfaces: A2uiSurfacePayload[];
  at: number;
}

export interface SessionMetaDto {
  session_id: string;
  title: string;
  status: 'ACTIVE' | 'SEALED' | string;
  turn_count: number;
  last_active_at: number;
  created_at: number;
}

export interface AgentSessionsBootstrapResp {
  active_session_id: string;
  sessions: SessionMetaDto[];
  turns: SessionTurnRecord[];
}

export interface LtmDiffOp {
  op: 'ADD' | 'REPLACE' | 'DELETE' | string;
  slot_key: string;
  old_val?: string | null;
  new_val?: string | null;
  reason?: string;
}

export interface LtmCommitDto {
  rev_id: string;
  scope: string;
  parent_rev?: string | null;
  source_session_id: string;
  message: string;
  ops: LtmDiffOp[];
  created_at: number;
}

export interface AgentMemoryInspectorResp {
  ltm_head: Array<{
    scope: string;
    slot_key: string;
    slot_value: string;
    current_rev: string;
    updated_at: number;
  }>;
  ltm_commits: LtmCommitDto[];
  stm_recent: Array<{
    id: string;
    scope: string;
    source_session_id: string;
    summary_text: string;
    promoted_to_ltm: boolean;
    created_at: number;
  }>;
  shared_markdown: string;
  private_markdown: string;
}

export interface ProviderConfigDto {
  id: string;
  vendor?: string;
  name: string;
  protocol: 'openai_compat' | 'anthropic' | 'gemini' | string;
  base_url: string;
  api_key: string;
  chat_model: string;
  memory_model: string;
  is_chat_primary: boolean;
  is_memory_primary: boolean;
  enabled: boolean;
  priority: number;
  health_status?: 'connected' | 'error' | 'unknown' | string;
  health_error?: string | null;
}

export interface LiveModelOption {
  id: string;
  label: string;
}

export interface FetchLiveModelsResp {
  connected: boolean;
  error?: string | null;
  resolved_base_url: string;
  models: LiveModelOption[];
}

export interface McpServerConfigDto {
  id: string;
  name: string;
  description: string;
  transport: 'http' | 'sse' | 'stdio' | string;
  endpoint: string;
  args: string[];
  headers: Record<string, string>;
  enabled: boolean;
  health_status?: 'connected' | 'auth_required' | 'error' | 'disabled' | 'unknown' | string;
  health_error?: string | null;
  tool_count?: number;
  tool_names?: string[];
}

export interface PetAgentEmotePayload {
  pose?: string;
  bubble_text?: string | null;
  for_user_id?: SnowflakeId;
}
