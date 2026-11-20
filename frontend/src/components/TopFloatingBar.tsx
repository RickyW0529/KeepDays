import React, { useState } from 'react';
import { AnimatePresence, motion } from 'framer-motion';
import {
  ArrowUp,
  Check,
  ChevronDown,
  Copy,
  Grid2x2,
  LayoutGrid,
  Lock,
  LogOut,
  MailOpen,
  Moon,
  Plus,
  Send,
  Sun,
  SunMoon,
  UserPlus,
  Users,
  X,
} from 'lucide-react';
import type { ThemePreference } from '../hooks/useBeijingAmbient';
import { api } from '../lib/apiClient';
import { resolveMediaUrl } from '../lib/mediaProcessor';
import { useOverlay } from '../hooks/useOverlay';
import type {
  AssetUrlMap,
  InvitationDto,
  SnowflakeId,
  SpaceDto,
  UserMap,
  UserSummaryDto,
} from '../types/api';

interface TopFloatingBarProps {
  currentUser: UserSummaryDto;
  spaces: SpaceDto[];
  activeSpace: SpaceDto;
  usersMap: UserMap;
  assetUrls: AssetUrlMap;
  pendingInvites: InvitationDto[];
  pendingNewCount: number;
  compactDensity: boolean;
  themePreference: ThemePreference;
  isNight: boolean;
  onCycleTheme: () => void;
  onSelectSpace: (spaceId: SnowflakeId) => void;
  onSpacesChanged: (selectSpaceId?: SnowflakeId) => void;
  onOpenMyProfile: () => void;
  onOpenMemberProfile: (uid: SnowflakeId) => void;
  onOpenUploadModal: () => void;
  onToggleDensity: () => void;
  onFlushPendingTimeline: () => void;
  onLogout: () => void;
}

export const TopFloatingBar: React.FC<TopFloatingBarProps> = ({
  currentUser,
  spaces,
  activeSpace,
  usersMap,
  assetUrls,
  pendingInvites,
  pendingNewCount,
  compactDensity,
  themePreference,
  isNight,
  onCycleTheme,
  onSelectSpace,
  onSpacesChanged,
  onOpenMyProfile,
  onOpenMemberProfile,
  onOpenUploadModal,
  onToggleDensity,
  onFlushPendingTimeline,
  onLogout,
}) => {
  const [menuOpen, setMenuOpen] = useState(false);
  const [newSpaceName, setNewSpaceName] = useState('');
  const [inviteDoorplate, setInviteDoorplate] = useState('');
  const [redeemCode, setRedeemCode] = useState('');
  const [generatedCode, setGeneratedCode] = useState<string | null>(null);
  const [copiedDoorplate, setCopiedDoorplate] = useState(false);
  const [feedback, setFeedback] = useState<string | null>(null);

  useOverlay(menuOpen, () => setMenuOpen(false));

  const myAvatar = resolveMediaUrl(currentUser.profile.avatar_asset_id, assetUrls, true);

  const handleCreateSpace = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!newSpaceName.trim()) return;
    try {
      const created = await api.createSharedSpace(newSpaceName.trim());
      setNewSpaceName('');
      setMenuOpen(false);
      onSpacesChanged(created.id);
    } catch (err) {
      setFeedback(err instanceof Error ? err.message : '创建失败');
    }
  };

  const handleSendInvite = async (e: React.FormEvent) => {
    e.preventDefault();
    try {
      const inv = await api.createInvitation(
        activeSpace.id,
        inviteDoorplate.trim() || undefined,
      );
      setGeneratedCode(inv.invite_code);
      setInviteDoorplate('');
      setFeedback(
        inv.invitee_id
          ? '邀请已发送，等待对方确认'
          : `一次性口令：${inv.invite_code}`,
      );
    } catch (err) {
      setFeedback(err instanceof Error ? err.message : '邀请失败');
    }
  };

  const handleRespondInvite = async (code: string, accept: boolean): Promise<boolean> => {
    try {
      const res = await api.respondInvitation(code, accept);
      onSpacesChanged(accept ? res.space_id : undefined);
      setFeedback(accept ? '已加入空间' : '已忽略邀请');
      return true;
    } catch (err) {
      setFeedback(err instanceof Error ? err.message : '操作失败');
      return false;
    }
  };

  const themeLabel =
    themePreference === 'auto'
      ? `自动昼夜（当前${isNight ? '夜间' : '白昼'}）`
      : themePreference === 'daylight'
        ? '白昼画廊'
        : '暗房夜色';

  return (
    <>
      <header
        style={{ paddingRight: 'calc(var(--kd-hdr-px, 0px) + var(--kd-sbw, 0px))' }}
        className="fixed top-0 inset-x-0 z-30 pointer-events-none pt-safe pl-3 sm:pl-6 lg:pl-10 [--kd-hdr-px:0.75rem] sm:[--kd-hdr-px:1.5rem] lg:[--kd-hdr-px:2.5rem]"
      >
        <div className="max-w-[1680px] mx-auto flex items-center justify-between gap-2 py-2">
          {/* Unified Architectural Left Capsule: Avatar + Space Selector */}
          <div className="pointer-events-auto inline-flex items-center h-9 p-1 pr-2.5 rounded-chip bg-panel/88 backdrop-blur-md border border-border shadow-xs">
            <button
              type="button"
              onClick={onOpenMyProfile}
              title="个人名片"
              className="w-7 h-7 rounded-chip bg-overlay border border-border flex items-center justify-center overflow-hidden hover:border-border-strong transition-colors cursor-pointer shrink-0"
            >
              {myAvatar ? (
                <img
                  src={myAvatar}
                  alt={currentUser.profile.nickname}
                  className="w-full h-full object-cover"
                />
              ) : (
                <span className="text-[11px] font-medium text-ink">
                  {currentUser.profile.nickname.slice(0, 1).toUpperCase()}
                </span>
              )}
            </button>

            <span aria-hidden className="w-px h-3.5 bg-border mx-2 shrink-0" />

            <button
              type="button"
              onClick={() => {
                setMenuOpen((v) => !v);
                setFeedback(null);
              }}
              className="inline-flex items-center gap-1.5 text-xs sm:text-[13px] font-medium text-ink hover:opacity-80 transition-opacity cursor-pointer"
            >
              {!activeSpace.is_shared && (
                <Lock className="w-3 h-3 text-muted shrink-0" />
              )}
              <span className="max-w-[128px] sm:max-w-[200px] truncate">
                {activeSpace.name}
              </span>
              {pendingInvites.length > 0 && (
                <span className="w-1.5 h-1.5 rounded-chip bg-accent animate-pulse" />
              )}
              <ChevronDown className="w-3.5 h-3.5 text-muted shrink-0" />
            </button>
          </div>

          {/* Center: Zero-Jank Pre-Decoded New Updates Pill */}
          <AnimatePresence>
            {pendingNewCount > 0 && (
              <motion.button
                type="button"
                initial={{ opacity: 0, y: -12 }}
                animate={{ opacity: 1, y: 0 }}
                exit={{ opacity: 0, y: -12 }}
                onClick={onFlushPendingTimeline}
                className="pointer-events-auto flex items-center gap-1.5 px-3.5 py-1.5 rounded-chip bg-ink text-surface text-xs font-medium shadow-float hover:opacity-90 transition-opacity cursor-pointer"
              >
                <ArrowUp className="w-3.5 h-3.5 text-accent" />
                <span>{pendingNewCount} 条新动态</span>
              </motion.button>
            )}
          </AnimatePresence>

          {/* Right Controls: Ambient Light + Gallery Density + Add Photo */}
          <div className="flex items-center gap-1.5 pointer-events-auto">
            <button
              type="button"
              onClick={onCycleTheme}
              title={`外观：${themeLabel}（点击切换）`}
              aria-label={`外观：${themeLabel}`}
              className="w-9 h-9 rounded-chip bg-panel/88 backdrop-blur-md border border-border shadow-xs flex items-center justify-center text-muted hover:text-ink hover:border-border-strong transition-colors cursor-pointer"
            >
              {themePreference === 'auto' ? (
                <SunMoon className="w-4 h-4" />
              ) : themePreference === 'daylight' ? (
                <Sun className="w-4 h-4" />
              ) : (
                <Moon className="w-4 h-4" />
              )}
            </button>

            <button
              type="button"
              onClick={onToggleDensity}
              title={compactDensity ? '切换为舒展画廊' : '切换为紧凑排列'}
              aria-label="切换画廊密度"
              className="w-9 h-9 rounded-chip bg-panel/88 backdrop-blur-md border border-border shadow-xs flex items-center justify-center text-muted hover:text-ink hover:border-border-strong transition-colors cursor-pointer"
            >
              {compactDensity ? (
                <LayoutGrid className="w-4 h-4" />
              ) : (
                <Grid2x2 className="w-4 h-4" />
              )}
            </button>

            <button
              type="button"
              onClick={onOpenUploadModal}
              className="flex items-center gap-1 h-9 px-3 sm:px-3.5 rounded-chip bg-ink text-surface text-xs font-medium shadow-xs hover:opacity-90 transition-opacity cursor-pointer"
            >
              <Plus className="w-3.5 h-3.5" />
              <span>照片</span>
            </button>
          </div>
        </div>
      </header>

      {/* Space Switcher, Doorplate & Co-Members Popover */}
      <AnimatePresence>
        {menuOpen && (
          <>
            <motion.div
              initial={{ opacity: 0 }}
              animate={{ opacity: 1 }}
              exit={{ opacity: 0 }}
              onClick={() => setMenuOpen(false)}
              className="fixed inset-0 z-40 bg-black/25 backdrop-blur-[2px]"
            />
            <motion.div
              initial={{ opacity: 0, y: -6, scale: 0.98 }}
              animate={{ opacity: 1, y: 0, scale: 1 }}
              exit={{ opacity: 0, y: -6, scale: 0.98 }}
              transition={{ duration: 0.16 }}
              className="fixed z-50 top-14 left-3 sm:left-6 lg:left-10 w-[calc(100vw-1.5rem)] max-w-[340px] max-h-[82dvh] overflow-y-auto bg-panel border border-border rounded-card shadow-float p-4 space-y-4"
            >
              {/* Doorplate Card */}
              <div className="flex items-center justify-between px-3 py-2.5 rounded-input bg-surface border border-border">
                <div>
                  <div className="text-[11px] text-muted">门牌号</div>
                  <div className="text-sm font-semibold tracking-wider text-ink tabular-nums">
                    #{currentUser.doorplate_no}
                  </div>
                </div>
                <button
                  type="button"
                  onClick={() => {
                    navigator.clipboard?.writeText(currentUser.doorplate_no);
                    setCopiedDoorplate(true);
                    setTimeout(() => setCopiedDoorplate(false), 1800);
                  }}
                  className="flex items-center gap-1 px-2.5 py-1 rounded-slip bg-panel border border-border text-xs text-ink hover:border-border-strong cursor-pointer"
                >
                  {copiedDoorplate ? (
                    <Check className="w-3 h-3 text-accent" />
                  ) : (
                    <Copy className="w-3 h-3" />
                  )}
                  <span>{copiedDoorplate ? '已复制' : '复制'}</span>
                </button>
              </div>

              {/* Pending Invitations Inbox */}
              {pendingInvites.length > 0 && (
                <div className="space-y-2">
                  <div className="flex items-center gap-1.5 text-xs font-medium text-accent">
                    <MailOpen className="w-3.5 h-3.5" />
                    <span>待确认邀请 ({pendingInvites.length})</span>
                  </div>
                  {pendingInvites.map((inv) => {
                    const inviter = usersMap[inv.inviter_id];
                    return (
                      <div
                        key={inv.id}
                        className="p-3 rounded-input bg-accent-soft border border-accent/25 space-y-2"
                      >
                        <div className="text-xs text-ink">
                          <span className="font-medium">
                            {inviter?.profile.nickname || '伙伴'}
                          </span>{' '}
                          邀请加入「{inv.space_name}」
                        </div>
                        <div className="flex gap-2">
                          <button
                            type="button"
                            onClick={() => handleRespondInvite(inv.invite_code, true)}
                            className="flex-1 py-1.5 rounded-slip bg-ink text-surface text-xs font-medium cursor-pointer"
                          >
                            加入
                          </button>
                          <button
                            type="button"
                            onClick={() => handleRespondInvite(inv.invite_code, false)}
                            className="px-3 py-1.5 rounded-slip bg-panel border border-border text-xs text-muted cursor-pointer"
                          >
                            忽略
                          </button>
                        </div>
                      </div>
                    );
                  })}
                </div>
              )}

              {/* Switch Space List */}
              <div className="space-y-1">
                <div className="text-[11px] text-muted px-1 mb-1">空间</div>
                {spaces.map((sp) => {
                  const isActive = sp.id === activeSpace.id;
                  return (
                    <button
                      key={sp.id}
                      type="button"
                      onClick={() => {
                        onSelectSpace(sp.id);
                        setMenuOpen(false);
                      }}
                      className={`w-full flex items-center justify-between px-3 py-2 rounded-input text-left text-xs sm:text-sm transition-colors cursor-pointer ${
                        isActive
                          ? 'bg-ink text-surface font-medium'
                          : 'hover:bg-overlay text-ink'
                      }`}
                    >
                      <div className="flex items-center gap-2 truncate">
                        {sp.is_shared ? (
                          <Users className="w-3.5 h-3.5 shrink-0 opacity-75" />
                        ) : (
                          <Lock className="w-3.5 h-3.5 shrink-0 opacity-75" />
                        )}
                        <span className="truncate">{sp.name}</span>
                      </div>
                      <span className="text-[11px] opacity-65 shrink-0 tabular-nums">
                        {sp.is_shared ? `${sp.members.length} 人` : '私密'}
                      </span>
                    </button>
                  );
                })}
              </div>

              {/* Active Shared Space Members & Invite Form */}
              {activeSpace.is_shared && (
                <div className="pt-3 border-t border-border space-y-2.5">
                  <div className="text-[11px] text-muted px-0.5">同住成员</div>
                  <div className="flex flex-wrap gap-1.5">
                    {activeSpace.members.map((m) => {
                      const u = usersMap[m.user_id];
                      return (
                        <button
                          key={m.user_id}
                          type="button"
                          onClick={() => {
                            setMenuOpen(false);
                            onOpenMemberProfile(m.user_id);
                          }}
                          className="px-2.5 py-1 rounded-chip bg-overlay text-xs text-ink hover:border-border-strong border border-transparent cursor-pointer"
                        >
                          {u?.profile.nickname || m.alias_name || '成员'}
                        </button>
                      );
                    })}
                  </div>

                  <form onSubmit={handleSendInvite} className="flex items-center gap-1.5">
                    <input
                      type="text"
                      value={inviteDoorplate}
                      onChange={(e) => setInviteDoorplate(e.target.value)}
                      placeholder="对方 #门牌号（留空生成口令）"
                      className="flex-1 min-w-0 h-8 px-2.5 text-xs bg-surface border border-border rounded-input text-ink placeholder:text-subtle focus:outline-none focus:border-border-strong"
                    />
                    <button
                      type="submit"
                      className="w-14 h-8 flex items-center justify-center gap-1 rounded-input bg-accent text-white text-xs font-medium cursor-pointer shrink-0"
                    >
                      <UserPlus className="w-3 h-3" />
                      <span>邀请</span>
                    </button>
                  </form>

                  {generatedCode && (
                    <div className="text-xs text-muted bg-overlay px-2.5 py-1.5 rounded-slip flex items-center justify-between">
                      <span className="tabular-nums">口令：{generatedCode}</span>
                      <button
                        type="button"
                        onClick={() => navigator.clipboard?.writeText(generatedCode)}
                        className="text-accent hover:underline cursor-pointer"
                      >
                        复制
                      </button>
                    </div>
                  )}
                </div>
              )}

              {/* Create New Shared Space & Redeem Invite Code */}
              <div className="pt-3 border-t border-border space-y-2">
                <form onSubmit={handleCreateSpace} className="flex items-center gap-1.5">
                  <input
                    type="text"
                    value={newSpaceName}
                    onChange={(e) => setNewSpaceName(e.target.value)}
                    placeholder="新建共享空间名称…"
                    className="flex-1 min-w-0 h-8 px-2.5 text-xs bg-surface border border-border rounded-input text-ink placeholder:text-subtle focus:outline-none focus:border-border-strong"
                  />
                  <button
                    type="submit"
                    className="w-14 h-8 flex items-center justify-center rounded-input bg-overlay hover:bg-border text-xs font-medium text-ink cursor-pointer shrink-0"
                  >
                    新建
                  </button>
                </form>

                <form
                  onSubmit={async (e) => {
                    e.preventDefault();
                    const code = redeemCode.trim();
                    if (!code) return;
                    const ok = await handleRespondInvite(code, true);
                    if (ok) setRedeemCode('');
                  }}
                  className="flex items-center gap-1.5"
                >
                  <input
                    type="text"
                    value={redeemCode}
                    onChange={(e) => setRedeemCode(e.target.value)}
                    placeholder="输入邀请口令加入…"
                    className="flex-1 min-w-0 h-8 px-2.5 text-xs bg-surface border border-border rounded-input text-ink placeholder:text-subtle focus:outline-none focus:border-border-strong"
                  />
                  <button
                    type="submit"
                    aria-label="兑换邀请口令"
                    className="w-14 h-8 flex items-center justify-center rounded-input bg-overlay hover:bg-border text-xs font-medium text-ink cursor-pointer shrink-0"
                  >
                    <Send className="w-3.5 h-3.5" />
                  </button>
                </form>
              </div>

              {feedback && (
                <div className="text-xs text-accent bg-accent-soft px-2.5 py-1.5 rounded-slip flex items-center justify-between gap-2">
                  <span>{feedback}</span>
                  <button
                    type="button"
                    onClick={() => setFeedback(null)}
                    className="cursor-pointer shrink-0"
                  >
                    <X className="w-3 h-3" />
                  </button>
                </div>
              )}

              <div className="pt-2.5 border-t border-border flex items-center justify-between">
                <button
                  type="button"
                  onClick={() => {
                    setMenuOpen(false);
                    onOpenMyProfile();
                  }}
                  className="text-xs text-muted hover:text-ink cursor-pointer"
                >
                  个人名片
                </button>
                <button
                  type="button"
                  onClick={onLogout}
                  className="flex items-center gap-1 text-xs text-muted hover:text-accent cursor-pointer"
                >
                  <LogOut className="w-3.5 h-3.5" />
                  <span>退出</span>
                </button>
              </div>
            </motion.div>
          </>
        )}
      </AnimatePresence>
    </>
  );
};
