import React, { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { AnimatePresence, motion } from 'framer-motion';
import {
  AlertCircle,
  BookOpen,
  Check,
  ChevronDown,
  Eye,
  EyeOff,
  FileEdit,
  History,
  Loader2,
  Lock,
  Plug,
  RefreshCw,
  RotateCcw,
  Save,
  Search,
  Trash2,
  Users,
  X,
  Zap,
} from 'lucide-react';
import { api } from '../lib/apiClient';
import { useOverlay } from '../hooks/useOverlay';
import { toast } from './Toaster';
import type {
  AgentMemoryInspectorResp,
  LiveModelOption,
  McpServerConfigDto,
  ProviderConfigDto,
  SnowflakeId,
} from '../types/api';

export type ToolboxTab = 'models' | 'memory' | 'mcp';

interface VendorPresetDef {
  id: string;
  name: string;
  badge: string;
  protocol: 'openai_compat' | 'anthropic' | 'gemini';
  tokenPlaceholder: string;
}

const VENDOR_PRESETS: VendorPresetDef[] = [
  { id: 'deepseek', name: 'DeepSeek', badge: 'DS', protocol: 'openai_compat', tokenPlaceholder: 'sk-...' },
  { id: 'minimax', name: 'MiniMax', badge: 'MM', protocol: 'openai_compat', tokenPlaceholder: 'API Token' },
  { id: 'kimi', name: 'Kimi', badge: 'KM', protocol: 'openai_compat', tokenPlaceholder: 'sk-...' },
  { id: 'zhipu', name: '智谱 GLM', badge: 'GLM', protocol: 'openai_compat', tokenPlaceholder: 'API Token' },
  { id: 'qwen', name: '通义千问', badge: 'QW', protocol: 'openai_compat', tokenPlaceholder: 'sk-...' },
  { id: 'doubao', name: '豆包', badge: 'DB', protocol: 'openai_compat', tokenPlaceholder: 'API Token' },
  { id: 'siliconflow', name: '硅基流动', badge: 'SF', protocol: 'openai_compat', tokenPlaceholder: 'sk-...' },
  { id: 'openrouter', name: 'OpenRouter', badge: 'OR', protocol: 'openai_compat', tokenPlaceholder: 'sk-or-...' },
  { id: 'anthropic', name: 'Claude', badge: 'CL', protocol: 'anthropic', tokenPlaceholder: 'sk-ant-...' },
  { id: 'openai', name: 'OpenAI', badge: 'OA', protocol: 'openai_compat', tokenPlaceholder: 'sk-...' },
  { id: 'gemini', name: 'Gemini', badge: 'GM', protocol: 'gemini', tokenPlaceholder: 'AIza...' },
  { id: 'xai', name: 'xAI Grok', badge: 'GK', protocol: 'openai_compat', tokenPlaceholder: 'xai-...' },
];

function matchVendorIdForProvider(p: ProviderConfigDto): string | null {
  const n = p.name.toLowerCase();
  const u = p.base_url.toLowerCase();
  if (n.includes('deepseek') || u.includes('deepseek')) return 'deepseek';
  if (n.includes('minimax') || u.includes('minimax')) return 'minimax';
  if (n.includes('kimi') || n.includes('moonshot') || u.includes('moonshot')) return 'kimi';
  if (n.includes('智谱') || n.includes('glm') || n.includes('zhipu') || u.includes('bigmodel') || u.includes('z.ai')) return 'zhipu';
  if (n.includes('千问') || n.includes('qwen') || u.includes('dashscope')) return 'qwen';
  if (n.includes('豆包') || n.includes('doubao') || u.includes('volces')) return 'doubao';
  if (n.includes('硅基') || n.includes('silicon') || u.includes('siliconflow')) return 'siliconflow';
  if (n.includes('openrouter') || u.includes('openrouter')) return 'openrouter';
  if (n.includes('claude') || n.includes('anthropic') || p.protocol === 'anthropic') return 'anthropic';
  if (n.includes('gemini') || p.protocol === 'gemini') return 'gemini';
  if (n.includes('grok') || n.includes('xai') || u.includes('x.ai')) return 'xai';
  if (n.includes('openai') || u.includes('api.openai.com')) return 'openai';
  return null;
}

interface PlatformToolboxProps {
  spaceId: SnowflakeId;
  isSharedSpace?: boolean;
  hidden?: boolean;
  open: boolean;
  initialTab?: ToolboxTab;
  externalRefreshSeq?: number;
  onOpen: (tab?: ToolboxTab) => void;
  onClose: () => void;
}

/**
 * Cute Chubby Miniature Toolbox (Zero Text · Zero Clipping · Hover Pops Open & Peeks Chubby Tools!)
 * Strict back-to-front SVG layer stack + `<clipPath>` pocket guarantees 0% mesh/stroke penetration.
 */
const PhysicalToolboxButton: React.FC<{
  status: 'connected' | 'error' | 'idle';
  onClick: () => void;
}> = ({ status, onClick }) => {
  const ledColor =
    status === 'connected'
      ? '#10B981'
      : status === 'error'
        ? '#F43F5E'
        : '#D4B483';

  return (
    <button
      type="button"
      onClick={onClick}
      aria-label="打开工具箱"
      className="group relative w-[66px] h-[66px] sm:w-[72px] sm:h-[70px] flex items-end justify-center cursor-pointer select-none outline-none focus-visible:ring-2 focus-visible:ring-accent rounded-[16px] transition-transform duration-200 hover:-translate-y-0.5 active:scale-95 transform-gpu"
    >
      <svg
        viewBox="0 0 80 74"
        fill="none"
        xmlns="http://www.w3.org/2000/svg"
        className="w-full h-full overflow-visible"
      >
        <defs>
          {/* Clips peeking tools strictly at the front chest lip (y=36) so shafts never poke out below! */}
          <clipPath id="kdCuteToolPocket">
            <rect x="8" y="-12" width="64" height="48" />
          </clipPath>

          <linearGradient id="kdCuteLidGrad" x1="15" y1="20" x2="65" y2="36" gradientUnits="userSpaceOnUse">
            <stop offset="0%" stopColor="#E59880" />
            <stop offset="100%" stopColor="var(--kd-accent)" />
          </linearGradient>

          <linearGradient id="kdCuteBodyGrad" x1="14" y1="35" x2="66" y2="62" gradientUnits="userSpaceOnUse">
            <stop offset="0%" stopColor="var(--kd-fridge-hi)" />
            <stop offset="65%" stopColor="var(--kd-fridge)" />
            <stop offset="100%" stopColor="var(--kd-fridge-edge)" />
          </linearGradient>

          <linearGradient id="kdCuteMetal" x1="0" y1="0" x2="1" y2="1">
            <stop offset="0%" stopColor="#FCFBF8" />
            <stop offset="60%" stopColor="#D7D0C5" />
            <stop offset="100%" stopColor="#A9A094" />
          </linearGradient>

          <linearGradient id="kdCuteClasp" x1="34" y1="38" x2="46" y2="49" gradientUnits="userSpaceOnUse">
            <stop offset="0%" stopColor="#FAF6EE" />
            <stop offset="100%" stopColor="#CFC5B6" />
          </linearGradient>
        </defs>

        {/* LAYER 1: Cozy Soft Floor Shadow */}
        <ellipse
          cx="40"
          cy="65.5"
          rx="23"
          ry="4"
          fill="rgba(24, 20, 16, 0.16)"
          className="transition-opacity duration-300 group-hover:opacity-75"
        />

        {/* LAYER 2 (BACK): Cute Rounded Terracotta Lid + Chubby Handle
            Sits BEHIND the tools so when it lifts up on hover, tools pop up cleanly in front of it! */}
        <g
          style={{ transformBox: 'fill-box' }}
          className="transition-transform duration-300 ease-[cubic-bezier(0.34,1.45,0.64,1)] group-hover:-translate-y-[58%]"
        >
          {/* Chubby Rounded Carrying Handle */}
          <path
            d="M30 23.5V19C30 16.8 31.8 15 34 15H46C48.2 15 50 16.8 50 19V23.5"
            stroke="#5C473E"
            strokeWidth="3.8"
            strokeLinecap="round"
          />
          <path
            d="M30 23.5V19C30 16.8 31.8 15 34 15H46C48.2 15 50 16.8 50 19V23.5"
            stroke="#F4EDE2"
            strokeWidth="1.6"
            strokeLinecap="round"
          />

          {/* Plump Dome Lid */}
          <path
            d="M15 29C15 24.6 18.6 21 23 21H57C61.4 21 65 24.6 65 29V36.5H15V29Z"
            fill="url(#kdCuteLidGrad)"
            stroke="#5C473E"
            strokeWidth="1.4"
            strokeLinejoin="round"
          />
          {/* Cute Glossy Pill Highlight on Lid Cheek */}
          <path
            d="M21.5 25.2C24 24.2 28.5 24 32.5 24"
            stroke="rgba(255,255,255,0.55)"
            strokeWidth="2"
            strokeLinecap="round"
          />
        </g>

        {/* LAYER 3: Cozy Inner Pocket Cavity (Bridges lifted lid & chest rim; 0% opacity when closed -> 0 bleed!) */}
        <rect
          x="16.2"
          y="22"
          width="47.6"
          height="15"
          rx="4"
          fill="#2B231E"
          stroke="#5C473E"
          strokeWidth="1.1"
          style={{ transformBox: 'fill-box' }}
          className="origin-bottom scale-y-0 opacity-0 transition-all duration-250 ease-out group-hover:scale-y-100 group-hover:opacity-100"
        />

        {/* LAYER 4: Cute Chubby Tools Inside Pocket (Clipped strictly above y=36, in front of open lid!) */}
        <g clipPath="url(#kdCuteToolPocket)">
          {/* Left: Chubby Rounded Silver Open-End Wrench (可爱圆胖小扳手) */}
          <g
            style={{ transformBox: 'fill-box' }}
            className="origin-bottom translate-y-full opacity-0 transition-all duration-300 ease-[cubic-bezier(0.34,1.52,0.64,1)] group-hover:translate-y-0 group-hover:-rotate-[13deg] group-hover:opacity-100"
          >
            <rect
              x="28.2"
              y="22"
              width="5.6"
              height="17"
              rx="2.6"
              fill="url(#kdCuteMetal)"
              stroke="#5C534A"
              strokeWidth="1.25"
            />
            <path
              d="M25.2 17.2C23 19.6 23.6 23.6 27 25H35C38.4 23.6 39 19.6 36.8 17.2L34.1 21H27.9L25.2 17.2Z"
              fill="url(#kdCuteMetal)"
              stroke="#5C534A"
              strokeWidth="1.25"
              strokeLinejoin="round"
            />
          </g>

          {/* Right: Cute Honey-Handled Stubby Screwdriver (奶黄胖手柄小螺丝刀) */}
          <g
            style={{ transformBox: 'fill-box' }}
            className="origin-bottom translate-y-full opacity-0 transition-all duration-300 delay-[35ms] ease-[cubic-bezier(0.34,1.52,0.64,1)] group-hover:translate-y-0 group-hover:rotate-[12deg] group-hover:opacity-100"
          >
            <rect
              x="47.5"
              y="25"
              width="3"
              height="13.5"
              rx="1.2"
              fill="#E5DFD5"
              stroke="#5C534A"
              strokeWidth="1.15"
            />
            <rect
              x="44.2"
              y="16"
              width="9.6"
              height="10.8"
              rx="4.6"
              fill="#F3C774"
              stroke="#5C473E"
              strokeWidth="1.25"
            />
            <line
              x1="47.2"
              y1="18.8"
              x2="47.2"
              y2="23.8"
              stroke="rgba(255,255,255,0.65)"
              strokeWidth="1.3"
              strokeLinecap="round"
            />
          </g>
        </g>

        {/* LAYER 5 (FRONT): Plump Cream Enamel Lower Chest + Cute Feet + Rounded Buckle Clasp */}
        <g>
          {/* Little Cute Rounded Feet */}
          <rect x="20" y="59" width="8" height="4.5" rx="2.2" fill="#5C473E" />
          <rect x="52" y="59" width="8" height="4.5" rx="2.2" fill="#5C473E" />

          {/* Chubby Rounded Chest Body */}
          <path
            d="M14 36.5C14 35.4 14.9 34.5 16 34.5H64C65.1 34.5 66 35.4 66 36.5V52C66 57 62 61 57 61H23C18 61 14 57 14 52V36.5Z"
            fill="url(#kdCuteBodyGrad)"
            stroke="#5C473E"
            strokeWidth="1.4"
            strokeLinejoin="round"
          />

          {/* Top Cream Rim Highlight */}
          <line
            x1="17"
            y1="36.4"
            x2="63"
            y2="36.4"
            stroke="rgba(255,255,255,0.72)"
            strokeWidth="1.3"
            strokeLinecap="round"
          />

          {/* Cute Warm Rosy Cheek Rivets */}
          <circle cx="21.5" cy="43.5" r="1.7" fill="#DF9A8C" opacity="0.72" />
          <circle cx="58.5" cy="43.5" r="1.7" fill="#DF9A8C" opacity="0.72" />

          {/* Cute Chubby Pill Buckle Clasp (Safely below y=34.5 rim -> 0% clipping!) */}
          <rect
            x="34.5"
            y="38.2"
            width="11"
            height="11"
            rx="4"
            fill="url(#kdCuteClasp)"
            stroke="#5C473E"
            strokeWidth="1.25"
          />
          <circle cx="40" cy="43.7" r="2.5" fill={ledColor} />
        </g>
      </svg>
    </button>
  );
};

/** Custom Dropdown for Official Live Models (Never manual typing!) */
const LiveModelDropdown: React.FC<{
  value: string;
  options: LiveModelOption[];
  loading: boolean;
  disabled?: boolean;
  onChange: (modelId: string) => void;
  onRefresh: () => void;
}> = ({ value, options, loading, disabled, onChange, onRefresh }) => {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState('');
  const wrapRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    if (!open) return;
    const onDocClick = (e: MouseEvent) => {
      if (wrapRef.current && !wrapRef.current.contains(e.target as Node)) {
        setOpen(false);
      }
    };
    document.addEventListener('mousedown', onDocClick);
    return () => document.removeEventListener('mousedown', onDocClick);
  }, [open]);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return options;
    return options.filter(
      (m) => m.id.toLowerCase().includes(q) || m.label.toLowerCase().includes(q),
    );
  }, [options, query]);

  const selectedLabel =
    options.find((m) => m.id === value)?.label ||
    value ||
    (loading ? '正在拉取官方模型…' : '填入 Token 自动拉取模型列表');

  return (
    <div ref={wrapRef} className="relative flex items-center gap-1.5">
      <button
        type="button"
        disabled={disabled || (options.length === 0 && !loading)}
        onClick={() => setOpen((o) => !o)}
        className="flex-1 min-w-0 h-9 px-3 rounded-input bg-panel border border-border hover:border-border-strong disabled:opacity-55 flex items-center justify-between gap-2 text-xs text-ink transition-colors cursor-pointer disabled:cursor-default"
      >
        <span className="truncate font-mono">{selectedLabel}</span>
        {loading ? (
          <Loader2 className="w-3.5 h-3.5 text-muted animate-spin shrink-0" />
        ) : (
          <ChevronDown
            className={`w-3.5 h-3.5 text-muted shrink-0 transition-transform ${
              open ? 'rotate-180' : ''
            }`}
          />
        )}
      </button>

      <button
        type="button"
        onClick={onRefresh}
        disabled={loading || disabled}
        aria-label="刷新模型列表"
        className="w-9 h-9 rounded-input bg-panel border border-border hover:border-border-strong disabled:opacity-45 flex items-center justify-center text-muted hover:text-ink transition-colors cursor-pointer shrink-0"
      >
        <RefreshCw className={`w-3.5 h-3.5 ${loading ? 'animate-spin' : ''}`} />
      </button>

      <AnimatePresence>
        {open && options.length > 0 && (
          <motion.div
            initial={{ opacity: 0, y: 6, scale: 0.99 }}
            animate={{ opacity: 1, y: 0, scale: 1 }}
            exit={{ opacity: 0, y: 6, scale: 0.99 }}
            transition={{ duration: 0.14, ease: [0.22, 1, 0.36, 1] }}
            className="absolute z-40 left-0 right-10 bottom-full mb-2 rounded-input bg-panel border border-border-strong shadow-float overflow-hidden"
          >
            {options.length > 8 && (
              <div className="p-2 border-b border-border flex items-center gap-2 bg-surface/65">
                <Search className="w-3.5 h-3.5 text-subtle shrink-0" />
                <input
                  type="text"
                  value={query}
                  onChange={(e) => setQuery(e.target.value)}
                  placeholder="筛选模型…"
                  className="w-full bg-transparent text-xs text-ink placeholder:text-subtle focus:outline-none"
                />
              </div>
            )}
            <div className="max-h-48 overflow-y-auto p-1">
              {filtered.length === 0 ? (
                <div className="py-3 text-center text-xs text-subtle">无匹配模型</div>
              ) : (
                filtered.map((m) => {
                  const active = m.id === value;
                  return (
                    <button
                      key={m.id}
                      type="button"
                      onClick={() => {
                        onChange(m.id);
                        setOpen(false);
                        setQuery('');
                      }}
                      className={`w-full px-2.5 py-2 rounded-[6px] text-left text-xs flex items-center justify-between gap-2 transition-colors cursor-pointer ${
                        active
                          ? 'bg-accent-soft text-accent font-medium'
                          : 'text-ink hover:bg-overlay'
                      }`}
                    >
                      <span className="truncate font-mono">{m.label}</span>
                      {active && <Check className="w-3.5 h-3.5 shrink-0" />}
                    </button>
                  );
                })
              )}
            </div>
          </motion.div>
        )}
      </AnimatePresence>
    </div>
  );
};

/** Clean Markdown preview renderer */
const MarkdownMemoryPreview: React.FC<{ content: string }> = ({ content }) => {
  const lines = content
    .replace(/<!--[\s\S]*?-->/g, '')
    .split('\n')
    .map((l) => l.trimEnd());

  const nonEmpty = lines.some((l) => {
    const t = l.trim();
    return t && !t.startsWith('# ');
  });

  if (!nonEmpty) {
    return (
      <div className="py-12 text-center text-xs text-subtle select-none">
        暂无内容
      </div>
    );
  }

  return (
    <div className="space-y-2 text-xs text-ink leading-relaxed">
      {lines.map((raw, idx) => {
        const line = raw.trim();
        if (!line) return <div key={idx} className="h-1" />;
        if (line.startsWith('# ')) {
          return (
            <h4 key={idx} className="font-editorial text-base text-ink pb-1 border-b border-border">
              {line.slice(2)}
            </h4>
          );
        }
        if (line.startsWith('## ') || line.startsWith('### ')) {
          return (
            <h5 key={idx} className="text-xs font-semibold text-ink pt-2">
              {line.replace(/^#{2,3}\s+/, '')}
            </h5>
          );
        }
        const bullet = line.replace(/^[-*•]\s+/, '');
        const boldMatch = bullet.match(/^\*\*(.+?)\*\*[:：]\s*(.+)$/);
        if (boldMatch) {
          return (
            <div key={idx} className="flex items-baseline gap-2 pl-1">
              <span className="w-1 h-1 rounded-full bg-accent shrink-0 translate-y-[-2px]" />
              <div>
                <span className="font-medium text-ink">{boldMatch[1]}</span>
                <span className="text-muted mx-1">·</span>
                <span className="text-ink/90">{boldMatch[2]}</span>
              </div>
            </div>
          );
        }
        return (
          <div key={idx} className="flex items-baseline gap-2 pl-1">
            <span className="w-1 h-1 rounded-full bg-muted shrink-0 translate-y-[-2px]" />
            <span className="text-ink/90">{bullet}</span>
          </div>
        );
      })}
    </div>
  );
};

export const PlatformToolbox: React.FC<PlatformToolboxProps> = ({
  spaceId,
  hidden = false,
  open,
  initialTab = 'models',
  externalRefreshSeq = 0,
  onOpen,
  onClose,
}) => {
  const [tab, setTab] = useState<ToolboxTab>(initialTab);
  const [providers, setProviders] = useState<ProviderConfigDto[]>([]);
  const [mcpServers, setMcpServers] = useState<McpServerConfigDto[]>([]);
  const [memData, setMemData] = useState<AgentMemoryInspectorResp | null>(null);

  // Vendor Selection & Official Live Model Dropdown State
  const [selectedVendorId, setSelectedVendorId] = useState<string>('deepseek');
  const [liveModels, setLiveModels] = useState<LiveModelOption[]>([]);
  const [loadingModels, setLoadingModels] = useState(false);
  const [liveProbeError, setLiveProbeError] = useState<string | null>(null);
  const [selectedModel, setSelectedModel] = useState<string>('');
  const [apiToken, setApiToken] = useState('');
  const [showToken, setShowToken] = useState(false);
  const [savingProvider, setSavingProvider] = useState(false);

  // Markdown Memory State
  const [memScope, setMemScope] = useState<'shared' | 'private'>('shared');
  const [memEditMode, setMemEditMode] = useState<'edit' | 'preview'>('edit');
  const [sharedDraft, setSharedDraft] = useState('');
  const [privateDraft, setPrivateDraft] = useState('');
  const [memDirty, setMemDirty] = useState(false);
  const [savingMem, setSavingMem] = useState(false);
  const [showHistory, setShowHistory] = useState(false);

  // MCP State
  const [mcpName, setMcpName] = useState('');
  const [mcpEndpoint, setMcpEndpoint] = useState('');
  const [mcpAuthText, setMcpAuthText] = useState('');
  const [showMcpAuth, setShowMcpAuth] = useState(false);
  const [expandedMcpId, setExpandedMcpId] = useState<string | null>(null);
  const [probingMcpId, setProbingMcpId] = useState<string | null>(null);
  const [savingMcp, setSavingMcp] = useState(false);

  useOverlay(open, onClose);

  useEffect(() => {
    if (open && initialTab) setTab(initialTab);
  }, [open, initialTab]);

  // Map vendorId -> saved ProviderConfigDto
  const vendorProviderMap = useMemo(() => {
    const map = new Map<string, ProviderConfigDto>();
    for (const p of providers) {
      const vid = matchVendorIdForProvider(p);
      if (vid && !map.has(vid)) {
        map.set(vid, p);
      }
    }
    return map;
  }, [providers]);

  const activeProvider = useMemo(
    () =>
      providers.find((p) => p.enabled && p.is_chat_primary) ||
      providers.find((p) => p.enabled) ||
      null,
    [providers],
  );

  const activeVendorId = useMemo(
    () => (activeProvider ? matchVendorIdForProvider(activeProvider) : null),
    [activeProvider],
  );

  const currentVendor = useMemo(
    () => VENDOR_PRESETS.find((v) => v.id === selectedVendorId) || VENDOR_PRESETS[0],
    [selectedVendorId],
  );

  const savedForSelected = vendorProviderMap.get(selectedVendorId) || null;
  const savedProviderId = savedForSelected?.id || '';
  const savedChatModel = savedForSelected?.chat_model || '';
  const isCurrentVendorActive =
    activeVendorId === selectedVendorId && Boolean(savedForSelected?.enabled);
  const lastFetchedKeyRef = useRef<string>('');
  const fetchReqSeqRef = useRef<number>(0);

  const refreshProviders = useCallback(async () => {
    try {
      const res = await api.getAgentProviders(spaceId);
      setProviders(res.providers);
      return res.providers;
    } catch {
      return [];
    }
  }, [spaceId]);

  const refreshMemory = useCallback(
    async (preserveDirty = false) => {
      try {
        const m = await api.getAgentMemory(spaceId);
        setMemData(m);
        if (!preserveDirty) {
          setSharedDraft(m.shared_markdown || '');
          setPrivateDraft(m.private_markdown || '');
          setMemDirty(false);
        }
      } catch {
        // ignore
      }
    },
    [spaceId],
  );

  const refreshMcp = useCallback(async () => {
    try {
      const m = await api.getAgentMcpServers(spaceId);
      setMcpServers(m.mcp_servers);
    } catch {
      // ignore
    }
  }, [spaceId]);

  useEffect(() => {
    void refreshProviders();
  }, [refreshProviders, externalRefreshSeq]);

  // When opening toolbox, default selection to the currently active vendor if any
  useEffect(() => {
    if (!open) {
      lastFetchedKeyRef.current = '';
      return;
    }
    void refreshProviders().then((list) => {
      const act =
        list.find((p) => p.enabled && p.is_chat_primary) || list.find((p) => p.enabled);
      const vid = act ? matchVendorIdForProvider(act) : null;
      if (vid) {
        setSelectedVendorId(vid);
        setSelectedModel(act?.chat_model || '');
      }
    });
    void refreshMemory(memDirty);
    void refreshMcp();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  // Pull live official models once per (vendor, providerId, token) tuple — zero infinite loops!
  const pullOfficialModels = useCallback(
    async (
      vendorId: string,
      tokenOverride?: string,
      providerId?: string,
      preferredModel?: string,
    ) => {
      const cleanOverride = (tokenOverride || '').trim();
      if (!cleanOverride && !providerId) {
        setLiveModels([]);
        setLiveProbeError(null);
        setLoadingModels(false);
        return;
      }
      const seq = ++fetchReqSeqRef.current;
      setLoadingModels(true);
      setLiveProbeError(null);
      try {
        const res = await api.fetchAgentVendorModels(spaceId, {
          vendor: vendorId,
          provider_id: providerId || undefined,
          api_key: cleanOverride || undefined,
        });
        if (seq !== fetchReqSeqRef.current) return;

        setLiveModels(res.models || []);
        if (!res.connected) {
          setLiveProbeError(res.error || '连接失败');
        } else {
          setLiveProbeError(null);
          setSelectedModel((prev) => {
            if (prev && res.models.some((m) => m.id === prev)) return prev;
            if (preferredModel && res.models.some((m) => m.id === preferredModel)) {
              return preferredModel;
            }
            return res.models[0]?.id || preferredModel || '';
          });
        }

        // Update cached health status in-memory only when actually changed (preserves array reference!)
        if (providerId) {
          const nextStatus = res.connected ? 'connected' : 'error';
          const nextErr = res.connected ? null : res.error || '连接失败';
          setProviders((prev) => {
            const target = prev.find((p) => p.id === providerId);
            if (
              !target ||
              (target.health_status === nextStatus && (target.health_error || null) === nextErr)
            ) {
              return prev;
            }
            return prev.map((p) =>
              p.id === providerId
                ? { ...p, health_status: nextStatus, health_error: nextErr }
                : p,
            );
          });
        }
      } catch (err) {
        if (seq !== fetchReqSeqRef.current) return;
        setLiveProbeError(err instanceof Error ? err.message : '拉取模型失败');
      } finally {
        if (seq === fetchReqSeqRef.current) {
          setLoadingModels(false);
        }
      }
    },
    [spaceId],
  );

  // Debounced auto-fetch when switching vendor or typing/pasting API Token
  useEffect(() => {
    if (!open || tab !== 'models') return;
    const trimmed = apiToken.trim();
    const fetchKey = `${selectedVendorId}|${savedProviderId}|${trimmed}`;

    if (trimmed.length >= 8) {
      if (lastFetchedKeyRef.current === fetchKey) return;
      const timer = window.setTimeout(() => {
        lastFetchedKeyRef.current = fetchKey;
        void pullOfficialModels(selectedVendorId, trimmed, savedProviderId, savedChatModel);
      }, 420);
      return () => window.clearTimeout(timer);
    }

    if (!trimmed && savedProviderId) {
      if (lastFetchedKeyRef.current === fetchKey) return;
      lastFetchedKeyRef.current = fetchKey;
      void pullOfficialModels(selectedVendorId, undefined, savedProviderId, savedChatModel);
    } else if (!trimmed) {
      lastFetchedKeyRef.current = '';
      setLiveModels([]);
      setLiveProbeError(null);
      setLoadingModels(false);
    }
  }, [apiToken, open, pullOfficialModels, savedChatModel, savedProviderId, selectedVendorId, tab]);

  const handleClickVendorCard = async (v: VendorPresetDef) => {
    setSelectedVendorId(v.id);
    setApiToken('');
    setLiveProbeError(null);
    const saved = vendorProviderMap.get(v.id);
    if (saved) {
      setSelectedModel(saved.chat_model);
      // If saved but not currently active, hot-switch immediately on click!
      if (!(saved.enabled && saved.is_chat_primary)) {
        try {
          const res = await api.activateAgentProvider(spaceId, saved.id);
          setProviders(res.providers);
        } catch {
          // ignore
        }
      }
    } else {
      setSelectedModel('');
      setLiveModels([]);
    }
  };

  const handleModelDropdownChange = async (nextModelId: string) => {
    setSelectedModel(nextModelId);
    // If already saved & no new token being typed, hot-swap model immediately!
    if (savedForSelected && !apiToken.trim()) {
      try {
        const res = await api.upsertAgentProvider(spaceId, {
          id: savedForSelected.id,
          vendor: currentVendor.id,
          name: currentVendor.name,
          protocol: currentVendor.protocol,
          api_key: savedForSelected.api_key,
          chat_model: nextModelId,
          memory_model: nextModelId,
          is_chat_primary: true,
          is_memory_primary: true,
          enabled: true,
          priority: 5,
        });
        setProviders(res.providers);
      } catch {
        toast('切换模型失败', 'error');
      }
    }
  };

  const handleSaveVendor = async (e: React.FormEvent) => {
    e.preventDefault();
    if (savingProvider) return;
    const cleanKey = apiToken.trim();
    if (!cleanKey && !savedForSelected) return;

    setSavingProvider(true);
    try {
      let modelToUse = selectedModel;
      if (!modelToUse) {
        const fetched = await api.fetchAgentVendorModels(spaceId, {
          vendor: currentVendor.id,
          provider_id: savedForSelected?.id,
          api_key: cleanKey || undefined,
        });
        setLiveModels(fetched.models || []);
        modelToUse = fetched.models[0]?.id || savedForSelected?.chat_model || '';
      }

      const res = await api.upsertAgentProvider(spaceId, {
        id: savedForSelected?.id,
        vendor: currentVendor.id,
        name: currentVendor.name,
        protocol: currentVendor.protocol,
        api_key: cleanKey || savedForSelected?.api_key || '',
        chat_model: modelToUse,
        memory_model: modelToUse,
        is_chat_primary: true,
        is_memory_primary: true,
        enabled: true,
        priority: 5,
      });
      setProviders(res.providers);
      setApiToken('');
      const updatedSelf = res.providers.find(
        (p) => matchVendorIdForProvider(p) === currentVendor.id,
      );
      if (updatedSelf?.chat_model) setSelectedModel(updatedSelf.chat_model);
      if (updatedSelf && liveModels.length === 0) {
        lastFetchedKeyRef.current = `${currentVendor.id}|${updatedSelf.id}|`;
        void pullOfficialModels(
          currentVendor.id,
          undefined,
          updatedSelf.id,
          updatedSelf.chat_model,
        );
      }
    } catch (err) {
      toast(err instanceof Error ? err.message : '保存失败', 'error');
    } finally {
      setSavingProvider(false);
    }
  };

  const handleDeleteSelectedVendor = async () => {
    if (!savedForSelected || savedForSelected.id.startsWith('env_')) return;
    try {
      const res = await api.deleteAgentProvider(spaceId, savedForSelected.id);
      setProviders(res.providers);
      setLiveModels([]);
      setSelectedModel('');
      setLiveProbeError(null);
    } catch {
      toast('移除失败', 'error');
    }
  };

  const handleSaveMarkdownMemory = useCallback(async () => {
    if (savingMem) return;
    setSavingMem(true);
    const content = memScope === 'shared' ? sharedDraft : privateDraft;
    try {
      const updated = await api.updateAgentMemoryMarkdown(spaceId, memScope, content);
      setMemData(updated);
      setSharedDraft(updated.shared_markdown || '');
      setPrivateDraft(updated.private_markdown || '');
      setMemDirty(false);
      toast('已保存', 'success');
    } catch (err) {
      toast(err instanceof Error ? err.message : '保存失败', 'error');
    } finally {
      setSavingMem(false);
    }
  }, [memScope, privateDraft, savingMem, sharedDraft, spaceId]);

  const toolboxLedStatus: 'connected' | 'error' | 'idle' = !activeProvider
    ? 'idle'
    : activeProvider.health_status === 'error'
      ? 'error'
      : 'connected';

  const activeErrorDetail =
    (savedForSelected?.id === activeProvider?.id ? liveProbeError : null) ||
    (activeProvider?.health_status === 'error' ? activeProvider.health_error : null);

  const activeMarkdown = memScope === 'shared' ? sharedDraft : privateDraft;

  return (
    <>
      {/* =====================================================================
          Bottom-Left Tactile Physical Toolbox (Zero Text · Hover Peeks Wrench!)
         ===================================================================== */}
      <div
        style={{
          bottom: 'max(1.25rem, env(safe-area-inset-bottom))',
          left: 'max(1.25rem, env(safe-area-inset-left))',
        }}
        className={`fixed z-30 transition-opacity duration-200 ${
          hidden || open ? 'opacity-0 pointer-events-none' : 'opacity-100'
        }`}
      >
        <PhysicalToolboxButton
          status={toolboxLedStatus}
          onClick={() => onOpen('models')}
        />
      </div>

      {/* =====================================================================
          Toolbox Modal (Clean · Slogan-Free · Official Live Model Dropdown)
         ===================================================================== */}
      <AnimatePresence>
        {open && (
          <motion.div
            initial={{ opacity: 0 }}
            animate={{ opacity: 1 }}
            exit={{ opacity: 0 }}
            transition={{ duration: 0.16 }}
            onClick={onClose}
            className="fixed inset-0 z-50 bg-black/40 backdrop-blur-xs flex items-end sm:items-center justify-center p-0 sm:p-6"
          >
            <motion.div
              initial={{ opacity: 0, y: 12, scale: 0.985 }}
              animate={{ opacity: 1, y: 0, scale: 1 }}
              exit={{ opacity: 0, y: 8, scale: 0.985 }}
              transition={{ duration: 0.18, ease: [0.22, 1, 0.36, 1] }}
              onClick={(e) => e.stopPropagation()}
              className="w-full sm:max-w-[640px] h-[84dvh] sm:h-auto sm:max-h-[78dvh] bg-panel border border-border-strong rounded-t-card sm:rounded-card shadow-float flex flex-col overflow-hidden transform-gpu"
            >
              {/* Minimalist Top Bar */}
              <div className="px-4 py-3 border-b border-border bg-surface/65 flex items-center justify-between gap-3 shrink-0">
                <div className="flex items-center gap-1 p-1 rounded-input bg-overlay/85 border border-border flex-1 max-w-[360px]">
                  {(
                    [
                      ['models', '模型', Zap],
                      ['memory', '记忆', BookOpen],
                      ['mcp', 'MCP', Plug],
                    ] as const
                  ).map(([key, label, Icon]) => {
                    const active = tab === key;
                    return (
                      <button
                        key={key}
                        type="button"
                        onClick={() => setTab(key)}
                        className={`flex-1 h-7 px-2.5 rounded-[6px] text-xs flex items-center justify-center gap-1.5 transition-all duration-150 cursor-pointer ${
                          active
                            ? 'bg-panel text-ink font-medium shadow-xs border border-border'
                            : 'text-muted hover:text-ink'
                        }`}
                      >
                        <Icon className="w-3.5 h-3.5 shrink-0" />
                        <span>{label}</span>
                      </button>
                    );
                  })}
                </div>

                <button
                  type="button"
                  onClick={onClose}
                  aria-label="关闭"
                  className="w-7 h-7 rounded-chip hover:bg-overlay flex items-center justify-center text-muted hover:text-ink transition-colors cursor-pointer shrink-0"
                >
                  <X className="w-4 h-4" />
                </button>
              </div>

              {/* Body */}
              <div className="flex-1 min-h-0 overflow-y-auto p-5">
                {tab === 'models' ? (
                  <div className="space-y-4">
                    {/* Unified 12-Vendor Grid: Active = Green (Connected) or Red + ! (Disconnected) */}
                    <div className="grid grid-cols-3 sm:grid-cols-4 gap-2">
                      {VENDOR_PRESETS.map((v) => {
                        const saved = vendorProviderMap.get(v.id);
                        const isActive = activeVendorId === v.id && Boolean(saved?.enabled);
                        const isSelected = selectedVendorId === v.id;
                        const isDisconnected =
                          isActive &&
                          (saved?.health_status === 'error' ||
                            (isSelected && Boolean(liveProbeError)));
                        const isConnected = isActive && !isDisconnected;

                        let cardStyle = 'bg-surface border-border hover:border-border-strong text-ink';
                        if (isConnected) {
                          cardStyle =
                            'bg-emerald-500/10 border-emerald-500 ring-1 ring-emerald-500/35 text-ink';
                        } else if (isDisconnected) {
                          cardStyle =
                            'bg-rose-500/10 border-rose-500 ring-1 ring-rose-500/35 text-ink';
                        } else if (isSelected) {
                          cardStyle = 'bg-panel border-ink ring-1 ring-ink/15 text-ink';
                        }

                        return (
                          <button
                            key={v.id}
                            type="button"
                            onClick={() => handleClickVendorCard(v)}
                            className={`h-12 px-3 rounded-input border transition-all duration-150 cursor-pointer flex items-center justify-between gap-2 ${cardStyle}`}
                          >
                            <div className="flex items-center gap-2 min-w-0">
                              <span
                                className={`w-6 h-6 rounded-[5px] text-[10px] font-semibold tracking-tight flex items-center justify-center shrink-0 ${
                                  isConnected
                                    ? 'bg-emerald-500 text-white'
                                    : isDisconnected
                                      ? 'bg-rose-500 text-white'
                                      : 'bg-overlay text-ink'
                                }`}
                              >
                                {v.badge}
                              </span>
                              <span className="text-xs font-medium truncate">{v.name}</span>
                            </div>

                            {isConnected ? (
                              <span className="w-2 h-2 rounded-full bg-emerald-500 shadow-[0_0_8px_rgba(16,185,129,0.65)] shrink-0" />
                            ) : isDisconnected ? (
                              <span
                                title={saved?.health_error || liveProbeError || '未连通'}
                                className="text-rose-500 shrink-0"
                              >
                                <AlertCircle className="w-4 h-4" />
                              </span>
                            ) : saved ? (
                              <span className="w-1.5 h-1.5 rounded-full bg-subtle shrink-0" />
                            ) : null}
                          </button>
                        );
                      })}
                    </div>

                    {/* Red Disconnection Alert Reason (Only shown when disconnected!) */}
                    {(activeErrorDetail || liveProbeError) && (
                      <div className="px-3.5 py-2.5 rounded-input bg-rose-500/10 border border-rose-500/35 text-xs text-rose-600 dark:text-rose-300 flex items-center justify-between gap-2">
                        <div className="flex items-center gap-2 min-w-0">
                          <AlertCircle className="w-4 h-4 shrink-0" />
                          <span className="truncate">{liveProbeError || activeErrorDetail}</span>
                        </div>
                        {activeProvider && (
                          <button
                            type="button"
                            onClick={async () => {
                              const res = await api.probeAgentProvider(spaceId, activeProvider.id);
                              setProviders(res.providers);
                            }}
                            className="px-2 py-0.5 rounded-chip bg-rose-500/15 hover:bg-rose-500/25 text-[11px] font-medium shrink-0 cursor-pointer"
                          >
                            重试
                          </button>
                        )}
                      </div>
                    )}

                    {/* Token + Live Official Model Dropdown */}
                    <form
                      onSubmit={handleSaveVendor}
                      className="p-4 rounded-card bg-surface border border-border space-y-3"
                    >
                      <div className="flex items-center justify-between">
                        <span className="text-xs font-medium text-ink">{currentVendor.name}</span>
                        {savedForSelected && !savedForSelected.id.startsWith('env_') && (
                          <button
                            type="button"
                            onClick={handleDeleteSelectedVendor}
                            className="inline-flex items-center gap-1 text-[11px] text-subtle hover:text-rose-500 transition-colors cursor-pointer"
                          >
                            <Trash2 className="w-3 h-3" />
                            <span>移除</span>
                          </button>
                        )}
                      </div>

                      <div className="relative">
                        <input
                          type={showToken ? 'text' : 'password'}
                          required={!savedForSelected}
                          value={apiToken}
                          onChange={(e) => setApiToken(e.target.value)}
                          placeholder={
                            savedForSelected
                              ? `${savedForSelected.api_key}（重新输入可替换 Token）`
                              : currentVendor.tokenPlaceholder
                          }
                          autoComplete="off"
                          className="w-full h-9 pl-3 pr-9 rounded-input bg-panel border border-border focus:border-border-strong text-xs text-ink placeholder:text-subtle focus:outline-none font-mono"
                        />
                        <button
                          type="button"
                          tabIndex={-1}
                          onClick={() => setShowToken((v) => !v)}
                          className="absolute right-2.5 top-1/2 -translate-y-1/2 text-subtle hover:text-ink cursor-pointer"
                        >
                          {showToken ? (
                            <EyeOff className="w-3.5 h-3.5" />
                          ) : (
                            <Eye className="w-3.5 h-3.5" />
                          )}
                        </button>
                      </div>

                      <div className="flex flex-col sm:flex-row gap-2">
                        <div className="flex-1 min-w-0">
                          <LiveModelDropdown
                            value={selectedModel || savedForSelected?.chat_model || ''}
                            options={
                              liveModels.length > 0
                                ? liveModels
                                : savedForSelected?.chat_model
                                  ? [
                                      {
                                        id: savedForSelected.chat_model,
                                        label: savedForSelected.chat_model,
                                      },
                                    ]
                                  : []
                            }
                            loading={loadingModels}
                            disabled={!apiToken.trim() && !savedForSelected}
                            onChange={handleModelDropdownChange}
                            onRefresh={() => {
                              lastFetchedKeyRef.current = '';
                              void pullOfficialModels(
                                selectedVendorId,
                                apiToken.trim() || undefined,
                                savedProviderId,
                                savedChatModel,
                              );
                            }}
                          />
                        </div>

                        <button
                          type="submit"
                          disabled={
                            (!apiToken.trim() && !savedForSelected) ||
                            (isCurrentVendorActive && !apiToken.trim()) ||
                            savingProvider
                          }
                          className={`h-9 px-4 rounded-input text-xs font-medium transition-all flex items-center justify-center gap-1.5 shrink-0 ${
                            isCurrentVendorActive && !apiToken.trim()
                              ? 'bg-emerald-500/12 text-emerald-700 dark:text-emerald-300 border border-emerald-500/30 cursor-default'
                              : 'bg-ink text-surface disabled:opacity-35 cursor-pointer'
                          }`}
                        >
                          {savingProvider ? (
                            <Loader2 className="w-3.5 h-3.5 animate-spin" />
                          ) : isCurrentVendorActive && !apiToken.trim() ? (
                            <Check className="w-3.5 h-3.5" />
                          ) : null}
                          <span>
                            {isCurrentVendorActive && !apiToken.trim()
                              ? '已启用'
                              : savedForSelected && !apiToken.trim()
                                ? '激活'
                                : '保存并启用'}
                          </span>
                        </button>
                      </div>
                    </form>
                  </div>
                ) : tab === 'memory' ? (
                  /* =========================================================
                     Tab 2: Clean Markdown Memory Editor (`SHARED.md` / `PRIVATE.md`)
                     ========================================================= */
                  <div className="flex flex-col space-y-3">
                    <div className="flex flex-wrap items-center justify-between gap-2">
                      <div className="inline-flex p-0.5 rounded-input bg-overlay border border-border">
                        <button
                          type="button"
                          onClick={() => setMemScope('shared')}
                          className={`px-3 py-1.5 rounded-[6px] text-xs flex items-center gap-1.5 transition-colors cursor-pointer ${
                            memScope === 'shared'
                              ? 'bg-panel text-ink font-medium shadow-2xs'
                              : 'text-muted hover:text-ink'
                          }`}
                        >
                          <Users className="w-3.5 h-3.5" />
                          <span>公共记忆</span>
                        </button>
                        <button
                          type="button"
                          onClick={() => setMemScope('private')}
                          className={`px-3 py-1.5 rounded-[6px] text-xs flex items-center gap-1.5 transition-colors cursor-pointer ${
                            memScope === 'private'
                              ? 'bg-panel text-ink font-medium shadow-2xs'
                              : 'text-muted hover:text-ink'
                          }`}
                        >
                          <Lock className="w-3.5 h-3.5" />
                          <span>私密记忆</span>
                        </button>
                      </div>

                      <div className="flex items-center gap-2">
                        <div className="inline-flex rounded-chip bg-surface border border-border p-0.5 text-[11px]">
                          <button
                            type="button"
                            onClick={() => setMemEditMode('edit')}
                            className={`px-2.5 py-1 rounded-chip flex items-center gap-1 cursor-pointer ${
                              memEditMode === 'edit'
                                ? 'bg-ink text-surface font-medium'
                                : 'text-muted hover:text-ink'
                            }`}
                          >
                            <FileEdit className="w-3 h-3" />
                            <span>编辑</span>
                          </button>
                          <button
                            type="button"
                            onClick={() => setMemEditMode('preview')}
                            className={`px-2.5 py-1 rounded-chip flex items-center gap-1 cursor-pointer ${
                              memEditMode === 'preview'
                                ? 'bg-ink text-surface font-medium'
                                : 'text-muted hover:text-ink'
                            }`}
                          >
                            <BookOpen className="w-3 h-3" />
                            <span>预览</span>
                          </button>
                        </div>

                        <button
                          type="button"
                          disabled={!memDirty || savingMem}
                          onClick={handleSaveMarkdownMemory}
                          className={`h-7 px-3 rounded-chip text-xs font-medium flex items-center gap-1.5 transition-all cursor-pointer ${
                            memDirty
                              ? 'bg-accent text-white shadow-xs'
                              : 'bg-overlay text-subtle cursor-default'
                          }`}
                        >
                          {savingMem ? (
                            <Loader2 className="w-3 h-3 animate-spin" />
                          ) : (
                            <Save className="w-3 h-3" />
                          )}
                          <span>保存</span>
                        </button>
                      </div>
                    </div>

                    {memEditMode === 'edit' ? (
                      <textarea
                        value={activeMarkdown}
                        onChange={(e) => {
                          if (memScope === 'shared') setSharedDraft(e.target.value);
                          else setPrivateDraft(e.target.value);
                          setMemDirty(true);
                        }}
                        onKeyDown={(e) => {
                          if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 's') {
                            e.preventDefault();
                            if (memDirty) handleSaveMarkdownMemory();
                          }
                        }}
                        spellCheck={false}
                        rows={11}
                        placeholder="# 记忆&#10;&#10;- 喜欢微辣，不吃香菜"
                        className="w-full p-4 rounded-card bg-paper-oat/45 dark:bg-surface border border-border focus:border-border-strong text-xs text-ink leading-relaxed font-mono resize-none focus:outline-none"
                      />
                    ) : (
                      <div className="min-h-[240px] max-h-[320px] p-4 rounded-card bg-paper-oat/35 dark:bg-surface border border-border overflow-y-auto">
                        <MarkdownMemoryPreview content={activeMarkdown} />
                      </div>
                    )}

                    {memData && memData.ltm_commits.length > 0 && (
                      <div className="border-t border-border pt-2">
                        <button
                          type="button"
                          onClick={() => setShowHistory((v) => !v)}
                          className="w-full flex items-center justify-between py-1 text-[11px] text-muted hover:text-ink transition-colors cursor-pointer"
                        >
                          <span className="flex items-center gap-1.5">
                            <History className="w-3.5 h-3.5" />
                            <span>修订记录 ({memData.ltm_commits.length})</span>
                          </span>
                          <ChevronDown
                            className={`w-3.5 h-3.5 transition-transform ${
                              showHistory ? 'rotate-180' : ''
                            }`}
                          />
                        </button>

                        <AnimatePresence>
                          {showHistory && (
                            <motion.div
                              initial={{ height: 0, opacity: 0 }}
                              animate={{ height: 'auto', opacity: 1 }}
                              exit={{ height: 0, opacity: 0 }}
                              className="overflow-hidden"
                            >
                              <div className="pt-2 space-y-1.5 max-h-40 overflow-y-auto">
                                {memData.ltm_commits.map((c) => (
                                  <div
                                    key={c.rev_id}
                                    className="px-3 py-2 rounded-input bg-surface border border-border flex items-center justify-between gap-2 text-[11px]"
                                  >
                                    <span className="text-ink truncate">
                                      {c.scope === 'shared' ? '[公共]' : '[私密]'} {c.message}
                                    </span>
                                    <button
                                      type="button"
                                      onClick={async () => {
                                        await api.revertAgentLtmCommit(spaceId, c.rev_id);
                                        await refreshMemory(false);
                                      }}
                                      className="px-2.5 py-1 rounded-chip bg-overlay hover:bg-accent-soft text-ink flex items-center gap-1 shrink-0 cursor-pointer"
                                    >
                                      <RotateCcw className="w-3 h-3" />
                                      <span>回退</span>
                                    </button>
                                  </div>
                                ))}
                              </div>
                            </motion.div>
                          )}
                        </AnimatePresence>
                      </div>
                    )}
                  </div>
                ) : (
                  /* =========================================================
                     Tab 3: Universal MCP Hot-Plug (HTTP / SSE / npx stdio / JSON)
                     ========================================================= */
                  <div className="space-y-3">
                    {mcpServers.length > 0 && (
                      <div className="space-y-2">
                        {mcpServers.map((m) => {
                          const isOk = m.health_status === 'connected';
                          const isErr =
                            m.health_status === 'error' || m.health_status === 'auth_required';
                          const isProbing = probingMcpId === m.id;
                          const isExpanded = expandedMcpId === m.id;
                          const toolList = m.tool_names || [];

                          return (
                            <div
                              key={m.id}
                              className={`p-3 rounded-input border transition-colors text-xs space-y-2 ${
                                isOk
                                  ? 'bg-emerald-500/6 border-emerald-500/45'
                                  : isErr
                                    ? 'bg-rose-500/6 border-rose-500/45'
                                    : 'bg-surface border-border'
                              }`}
                            >
                              <div className="flex items-center justify-between gap-2">
                                <div className="min-w-0 flex-1">
                                  <div className="flex items-center gap-2 min-w-0">
                                    <span className="font-medium text-ink truncate">{m.name}</span>
                                    {isOk ? (
                                      <button
                                        type="button"
                                        onClick={() =>
                                          toolList.length > 0 &&
                                          setExpandedMcpId(isExpanded ? null : m.id)
                                        }
                                        className="inline-flex items-center gap-1 px-1.5 py-0.5 rounded-chip bg-emerald-500/12 text-emerald-700 dark:text-emerald-300 text-[10px] font-medium shrink-0 cursor-pointer"
                                      >
                                        <span className="w-1.5 h-1.5 rounded-full bg-emerald-500" />
                                        <span>{m.tool_count || 0} 个工具</span>
                                        {toolList.length > 0 && (
                                          <ChevronDown
                                            className={`w-2.5 h-2.5 transition-transform ${
                                              isExpanded ? 'rotate-180' : ''
                                            }`}
                                          />
                                        )}
                                      </button>
                                    ) : isErr ? (
                                      <span className="inline-flex items-center gap-1 text-rose-500 text-[10px] font-medium shrink-0">
                                        <AlertCircle className="w-3.5 h-3.5" />
                                        <span>
                                          {m.health_status === 'auth_required'
                                            ? '需配置认证'
                                            : '未连通'}
                                        </span>
                                      </span>
                                    ) : null}
                                  </div>
                                  <div className="text-[11px] text-muted truncate font-mono mt-0.5">
                                    {m.transport === 'stdio' && m.args?.length
                                      ? `${m.endpoint} ${m.args.join(' ')}`
                                      : m.endpoint}
                                  </div>
                                </div>

                                <div className="flex items-center gap-1 shrink-0">
                                  <button
                                    type="button"
                                    disabled={isProbing}
                                    title="重新检测连接与工具列表"
                                    onClick={async () => {
                                      setProbingMcpId(m.id);
                                      try {
                                        const res = await api.probeAgentMcpServer(spaceId, m.id);
                                        setMcpServers(res.mcp_servers);
                                      } finally {
                                        setProbingMcpId(null);
                                      }
                                    }}
                                    className="p-1.5 rounded-chip text-subtle hover:text-ink hover:bg-overlay disabled:opacity-45 cursor-pointer"
                                  >
                                    <RefreshCw
                                      className={`w-3.5 h-3.5 ${isProbing ? 'animate-spin' : ''}`}
                                    />
                                  </button>
                                  <button
                                    type="button"
                                    title="移除"
                                    onClick={async () => {
                                      const res = await api.deleteAgentMcpServer(spaceId, m.id);
                                      setMcpServers(res.mcp_servers);
                                    }}
                                    className="p-1.5 rounded-chip text-subtle hover:text-rose-500 hover:bg-overlay cursor-pointer"
                                  >
                                    <Trash2 className="w-3.5 h-3.5" />
                                  </button>
                                </div>
                              </div>

                              {isErr && m.health_error && (
                                <div className="text-[11px] text-rose-600 dark:text-rose-300 leading-relaxed break-words">
                                  {m.health_error}
                                </div>
                              )}

                              {isExpanded && toolList.length > 0 && (
                                <div className="pt-1.5 border-t border-border/60 flex flex-wrap gap-1">
                                  {toolList.map((tn) => (
                                    <span
                                      key={tn}
                                      className="px-2 py-0.5 rounded-chip bg-panel border border-border text-[10px] font-mono text-muted"
                                    >
                                      {tn}
                                    </span>
                                  ))}
                                </div>
                              )}
                            </div>
                          );
                        })}
                      </div>
                    )}

                    <form
                      onSubmit={async (e) => {
                        e.preventDefault();
                        const rawTarget = mcpEndpoint.trim();
                        if (!rawTarget || savingMcp) return;

                        // Parse optional auth / env lines (`Bearer xxx` or `KEY=VALUE` or `Header: Value`)
                        const extraHeaders: Record<string, string> = {};
                        for (const line of mcpAuthText.split('\n')) {
                          const t = line.trim();
                          if (!t) continue;
                          const colonIdx = t.indexOf(':');
                          const eqIdx = t.indexOf('=');
                          if (eqIdx > 0 && (colonIdx < 0 || eqIdx < colonIdx)) {
                            extraHeaders[t.slice(0, eqIdx).trim()] = t.slice(eqIdx + 1).trim();
                          } else if (colonIdx > 0) {
                            extraHeaders[t.slice(0, colonIdx).trim()] = t
                              .slice(colonIdx + 1)
                              .trim();
                          } else {
                            extraHeaders['Authorization'] = t.startsWith('Bearer ')
                              ? t
                              : `Bearer ${t}`;
                          }
                        }

                        // Support pasting standard Claude/Cursor JSON (`{"mcpServers": {...}}` or single server JSON)
                        let configsToSave: McpServerConfigDto[] = [];
                        if (rawTarget.startsWith('{')) {
                          try {
                            const parsed = JSON.parse(rawTarget) as Record<string, unknown>;
                            const mapRoot =
                              parsed.mcpServers && typeof parsed.mcpServers === 'object'
                                ? (parsed.mcpServers as Record<string, Record<string, unknown>>)
                                : parsed.url || parsed.command || parsed.endpoint
                                  ? { [mcpName.trim() || 'mcp']: parsed as Record<string, unknown> }
                                  : (parsed as Record<string, Record<string, unknown>>);

                            for (const [k, v] of Object.entries(mapRoot)) {
                              if (!v || typeof v !== 'object') continue;
                              const urlVal =
                                (typeof v.url === 'string' && v.url) ||
                                (typeof v.serverUrl === 'string' && v.serverUrl) ||
                                (typeof v.endpoint === 'string' && v.endpoint) ||
                                '';
                              const cmdVal = typeof v.command === 'string' ? v.command : '';
                              const argsVal = Array.isArray(v.args)
                                ? v.args.map((x) => String(x))
                                : [];
                              const hdrObj = {
                                ...(typeof v.headers === 'object' && v.headers
                                  ? (v.headers as Record<string, string>)
                                  : {}),
                                ...(typeof v.env === 'object' && v.env
                                  ? (v.env as Record<string, string>)
                                  : {}),
                                ...extraHeaders,
                              };
                              if (urlVal) {
                                configsToSave.push({
                                  id: '',
                                  name: mcpName.trim() || k,
                                  description: '',
                                  transport: urlVal.endsWith('/sse') ? 'sse' : 'http',
                                  endpoint: urlVal,
                                  args: [],
                                  headers: hdrObj,
                                  enabled: true,
                                });
                              } else if (cmdVal) {
                                configsToSave.push({
                                  id: '',
                                  name: mcpName.trim() || k,
                                  description: '',
                                  transport: 'stdio',
                                  endpoint: cmdVal,
                                  args: argsVal,
                                  headers: hdrObj,
                                  enabled: true,
                                });
                              }
                            }
                          } catch {
                            toast('JSON 格式有误，请检查括号与引号', 'error');
                            return;
                          }
                        } else {
                          const isUrl =
                            rawTarget.startsWith('http://') || rawTarget.startsWith('https://');
                          const autoName =
                            mcpName.trim() ||
                            (isUrl
                              ? new URL(rawTarget).hostname.split('.')[0] || 'mcp'
                              : rawTarget
                                  .split(/\s+/)
                                  .pop()
                                  ?.replace(/^@[^/]+\//, '')
                                  .replace(/^server-|-mcp$/g, '') || 'mcp');

                          if (isUrl) {
                            configsToSave.push({
                              id: '',
                              name: autoName,
                              description: '',
                              transport: rawTarget.replace(/\/+$/, '').endsWith('/sse')
                                ? 'sse'
                                : 'http',
                              endpoint: rawTarget,
                              args: [],
                              headers: extraHeaders,
                              enabled: true,
                            });
                          } else {
                            const parts = rawTarget.split(/\s+/).filter(Boolean);
                            const prog = parts[0] || rawTarget;
                            const restArgs = parts.slice(1);
                            configsToSave.push({
                              id: '',
                              name: autoName,
                              description: '',
                              transport: 'stdio',
                              endpoint: prog,
                              args: restArgs,
                              headers: extraHeaders,
                              enabled: true,
                            });
                          }
                        }

                        if (configsToSave.length === 0) return;

                        setSavingMcp(true);
                        try {
                          let latestList = mcpServers;
                          for (const cfg of configsToSave) {
                            const res = await api.upsertAgentMcpServer(spaceId, cfg);
                            latestList = res.mcp_servers;
                          }
                          setMcpServers(latestList);
                          setMcpName('');
                          setMcpEndpoint('');
                          setMcpAuthText('');
                          setShowMcpAuth(false);
                        } catch (err) {
                          toast(err instanceof Error ? err.message : '添加失败', 'error');
                        } finally {
                          setSavingMcp(false);
                        }
                      }}
                      className="p-3.5 rounded-card bg-surface border border-border space-y-2.5"
                    >
                      <div className="flex flex-col sm:flex-row gap-2">
                        <input
                          type="text"
                          placeholder="名称（可选）"
                          value={mcpName}
                          onChange={(e) => setMcpName(e.target.value)}
                          className="sm:w-32 h-9 px-3 rounded-input bg-panel border border-border text-xs text-ink focus:outline-none focus:border-border-strong"
                        />
                        <input
                          type="text"
                          required
                          placeholder="https://... 或 npx -y @.../mcp 或粘贴 JSON"
                          value={mcpEndpoint}
                          onChange={(e) => setMcpEndpoint(e.target.value)}
                          className="flex-1 h-9 px-3 rounded-input bg-panel border border-border text-xs text-ink font-mono focus:outline-none focus:border-border-strong"
                        />
                        <button
                          type="submit"
                          disabled={!mcpEndpoint.trim() || savingMcp}
                          className="h-9 px-4 rounded-input bg-ink text-surface text-xs font-medium disabled:opacity-40 inline-flex items-center justify-center gap-1.5 cursor-pointer shrink-0"
                        >
                          {savingMcp && <Loader2 className="w-3.5 h-3.5 animate-spin" />}
                          <span>添加</span>
                        </button>
                      </div>

                      <div className="flex items-center justify-between">
                        <button
                          type="button"
                          onClick={() => setShowMcpAuth((v) => !v)}
                          className="text-[11px] text-muted hover:text-ink transition-colors cursor-pointer"
                        >
                          {showMcpAuth ? '收起认证参数 ▴' : '+ 认证 Token / 环境变量（如 Gmail、GitHub 等需授权服务）'}
                        </button>
                      </div>

                      {showMcpAuth && (
                        <textarea
                          rows={2}
                          value={mcpAuthText}
                          onChange={(e) => setMcpAuthText(e.target.value)}
                          placeholder={
                            '直接填入 Bearer Token，或每行一个 KEY=VALUE / Header: Value（如 GITHUB_PERSONAL_ACCESS_TOKEN=ghp_...）'
                          }
                          className="w-full p-2.5 rounded-input bg-panel border border-border focus:border-border-strong text-xs text-ink font-mono resize-none focus:outline-none"
                        />
                      )}
                    </form>
                  </div>
                )}
              </div>
            </motion.div>
          </motion.div>
        )}
      </AnimatePresence>
    </>
  );
};
