import React, { useEffect, useState } from 'react';
import { motion } from 'framer-motion';
import { Camera, Plus, Save, Trash2, X } from 'lucide-react';
import { api } from '../lib/apiClient';
import {
  ASSET_KIND,
  compressAndUploadImage,
  resolveMediaUrl,
} from '../lib/mediaProcessor';
import { useOverlay } from '../hooks/useOverlay';
import { toast } from './Toaster';
import type {
  AssetUrlMap,
  ProfileSection,
  SnowflakeId,
  UserProfileDoc,
  UserSummaryDto,
} from '../types/api';

interface ProfileDrawerProps {
  targetUserId: SnowflakeId;
  selfUserId: SnowflakeId;
  onClose: () => void;
  onSelfUpdated: (updated: UserSummaryDto, urls: AssetUrlMap) => void;
}

export const ProfileDrawer: React.FC<ProfileDrawerProps> = ({
  targetUserId,
  selfUserId,
  onClose,
  onSelfUpdated,
}) => {
  const isMe = targetUserId === selfUserId;
  const [userDto, setUserDto] = useState<UserSummaryDto | null>(null);
  const [urls, setUrls] = useState<AssetUrlMap>({});
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState<UserProfileDoc | null>(null);
  const [saving, setSaving] = useState(false);
  const [newItemInputs, setNewItemInputs] = useState<Record<string, string>>({});

  useOverlay(true, () => (editing ? setEditing(false) : onClose()));

  useEffect(() => {
    const load = async () => {
      const res = isMe
        ? await api.getMyProfile()
        : await api.getUserProfile(targetUserId);
      setUserDto(res.user);
      setUrls(res.asset_urls);
      setDraft(structuredClone(res.user.profile));
    };
    load().catch(() => {});
  }, [targetUserId, isMe]);

  const handleUploadAsset = async (file: File, kind: 'avatar' | 'banner') => {
    if (!draft) return;
    const uploaded = await compressAndUploadImage(
      file,
      kind === 'avatar' ? ASSET_KIND.AVATAR : ASSET_KIND.BANNER,
      kind === 'avatar',
    );
    const next = {
      ...draft,
      avatar_asset_id: kind === 'avatar' ? uploaded.assetId : draft.avatar_asset_id,
      banner_asset_id: kind === 'banner' ? uploaded.assetId : draft.banner_asset_id,
    };
    setDraft(next);
  };

  const handleSave = async () => {
    if (!draft) return;
    setSaving(true);
    try {
      const res = await api.updateMyProfile(draft);
      setUserDto(res.user);
      setUrls(res.asset_urls);
      setEditing(false);
      onSelfUpdated(res.user, res.asset_urls);
      toast('名片已更新', 'success');
    } catch (err) {
      toast(err instanceof Error ? err.message : '保存失败', 'error');
    } finally {
      setSaving(false);
    }
  };

  const addSection = () => {
    if (!draft) return;
    const nextSec: ProfileSection = {
      id: `sec_${Date.now()}`,
      title: '自定义模块',
      items: [],
      highlights: [],
    };
    setDraft({ ...draft, sections: [...draft.sections, nextSec] });
  };

  const activeDoc = editing && draft ? draft : userDto?.profile;
  const avatarUrl = resolveMediaUrl(activeDoc?.avatar_asset_id, urls, true);
  const bannerUrl = resolveMediaUrl(activeDoc?.banner_asset_id, urls, false);

  return (
    <motion.div
      initial={{ opacity: 0 }}
      animate={{ opacity: 1 }}
      exit={{ opacity: 0 }}
      transition={{ duration: 0.16, ease: 'easeOut' }}
      onClick={onClose}
      className="fixed inset-0 z-50 bg-black/50 backdrop-blur-xs flex justify-end items-end sm:items-stretch"
    >
      <motion.aside
        initial={{ x: 20, opacity: 0 }}
        animate={{ x: 0, opacity: 1 }}
        exit={{ x: 16, opacity: 0 }}
        transition={{ duration: 0.18, ease: [0.22, 1, 0.36, 1] }}
        onClick={(e) => e.stopPropagation()}
        className="w-full sm:w-[420px] h-[92dvh] sm:h-full bg-panel border-l border-border rounded-t-card sm:rounded-none shadow-float flex flex-col overflow-hidden transform-gpu"
      >
        {/* LinkedIn-Style Banner + Avatar Header */}
        <div className="relative h-36 bg-overlay shrink-0">
          {bannerUrl ? (
            <img src={bannerUrl} alt="Banner" className="w-full h-full object-cover" />
          ) : (
            <div className="w-full h-full bg-gradient-to-br from-overlay via-fridge to-surface" />
          )}

          {isMe && editing && (
            <label className="absolute bottom-2 right-3 px-2.5 py-1 rounded-chip bg-black/55 text-white text-[11px] flex items-center gap-1 cursor-pointer">
              <Camera className="w-3 h-3" />
              <span>换背景</span>
              <input
                type="file"
                accept="image/*"
                onChange={(e) => e.target.files?.[0] && handleUploadAsset(e.target.files[0], 'banner')}
                className="hidden"
              />
            </label>
          )}

          <button
            type="button"
            onClick={onClose}
            className="absolute top-3 right-3 w-8 h-8 rounded-chip bg-black/45 text-white flex items-center justify-center cursor-pointer"
          >
            <X className="w-4 h-4" />
          </button>

          {/* Avatar Overlapping Banner */}
          <div className="absolute -bottom-9 left-6 w-20 h-20 rounded-chip border-4 border-panel bg-surface overflow-hidden shadow-paper">
            {avatarUrl ? (
              <img src={avatarUrl} alt="Avatar" className="w-full h-full object-cover" />
            ) : (
              <div className="w-full h-full flex items-center justify-center text-xl font-editorial text-ink">
                {activeDoc?.nickname?.slice(0, 1) || 'K'}
              </div>
            )}
            {isMe && editing && (
              <label className="absolute inset-0 bg-black/45 text-white flex items-center justify-center cursor-pointer">
                <Camera className="w-5 h-5" />
                <input
                  type="file"
                  accept="image/*"
                  onChange={(e) => e.target.files?.[0] && handleUploadAsset(e.target.files[0], 'avatar')}
                  className="hidden"
                />
              </label>
            )}
          </div>
        </div>

        {/* Body */}
        <div className="flex-1 overflow-y-auto pt-12 px-6 pb-8 space-y-6">
          {!userDto || !activeDoc ? (
            <div className="text-xs text-muted py-8">正在加载名片档案...</div>
          ) : (
            <>
              <div className="flex items-start justify-between gap-3">
                <div className="flex-1 space-y-1.5">
                  {editing && draft ? (
                    <>
                      <input
                        type="text"
                        value={draft.nickname}
                        onChange={(e) => setDraft({ ...draft, nickname: e.target.value })}
                        placeholder="昵称"
                        className="w-full px-2.5 py-1.5 text-base font-semibold bg-surface border border-border rounded-input text-ink"
                      />
                      <input
                        type="text"
                        value={draft.signature}
                        onChange={(e) => setDraft({ ...draft, signature: e.target.value })}
                        placeholder="写一句当下签名或生活状态..."
                        className="w-full px-2.5 py-1.5 text-xs bg-surface border border-border rounded-input text-muted"
                      />
                    </>
                  ) : (
                    <>
                      <div className="flex items-center gap-2">
                        <h2 className="text-xl font-semibold text-ink">{activeDoc.nickname}</h2>
                        <span className="px-2 py-0.5 rounded-chip bg-overlay text-[11px] text-muted">
                          #{userDto.doorplate_no}
                        </span>
                      </div>
                      <p className="text-xs text-muted leading-relaxed">
                        {activeDoc.signature || '还没有写下签名'}
                      </p>
                    </>
                  )}
                </div>

                {isMe && (
                  <div>
                    {editing ? (
                      <button
                        type="button"
                        disabled={saving}
                        onClick={handleSave}
                        className="flex items-center gap-1 px-3 py-1.5 rounded-chip bg-ink text-surface text-xs font-medium cursor-pointer"
                      >
                        <Save className="w-3.5 h-3.5" />
                        <span>{saving ? '保存中' : '保存'}</span>
                      </button>
                    ) : (
                      <button
                        type="button"
                        onClick={() => setEditing(true)}
                        className="px-3 py-1.5 rounded-chip border border-border text-xs text-ink hover:border-border-strong cursor-pointer"
                      >
                        编辑名片
                      </button>
                    )}
                  </div>
                )}
              </div>

              {/* Modular LinkedIn-Style Sections */}
              <div className="space-y-4">
                {activeDoc.sections.map((sec, sIdx) => (
                  <div
                    key={sec.id}
                    className="p-4 rounded-card bg-surface border border-border space-y-2.5"
                  >
                    <div className="flex items-center justify-between gap-2">
                      {editing && draft ? (
                        <input
                          type="text"
                          value={sec.title}
                          onChange={(e) => {
                            const nextSecs = [...draft.sections];
                            nextSecs[sIdx] = { ...sec, title: e.target.value };
                            setDraft({ ...draft, sections: nextSecs });
                          }}
                          className="flex-1 text-xs font-semibold bg-panel px-2 py-1 rounded-slip border border-border text-ink"
                        />
                      ) : (
                        <h4 className="text-xs font-semibold tracking-wide text-ink">
                          {sec.title}
                        </h4>
                      )}

                      {editing && draft && (
                        <button
                          type="button"
                          onClick={() => {
                            setDraft({
                              ...draft,
                              sections: draft.sections.filter((_, i) => i !== sIdx),
                            });
                          }}
                          className="text-subtle hover:text-accent cursor-pointer"
                        >
                          <Trash2 className="w-3.5 h-3.5" />
                        </button>
                      )}
                    </div>

                    <div className="flex flex-wrap gap-1.5">
                      {sec.items.length === 0 && !editing && (
                        <span className="text-xs text-subtle">暂未填写条目</span>
                      )}
                      {sec.items.map((item, iIdx) => (
                        <span
                          key={iIdx}
                          className="inline-flex items-center gap-1 px-2.5 py-1 rounded-chip bg-panel border border-border text-xs text-ink"
                        >
                          {item}
                          {editing && draft && (
                            <button
                              type="button"
                              onClick={() => {
                                const nextSecs = [...draft.sections];
                                nextSecs[sIdx] = {
                                  ...sec,
                                  items: sec.items.filter((_, j) => j !== iIdx),
                                };
                                setDraft({ ...draft, sections: nextSecs });
                              }}
                              className="text-subtle hover:text-accent ml-0.5"
                            >
                              ×
                            </button>
                          )}
                        </span>
                      ))}
                    </div>

                    {editing && draft && (
                      <div className="flex gap-1.5 pt-1">
                        <input
                          type="text"
                          value={newItemInputs[sec.id] || ''}
                          onChange={(e) =>
                            setNewItemInputs({ ...newItemInputs, [sec.id]: e.target.value })
                          }
                          onKeyDown={(e) => {
                            if (e.key === 'Enter') {
                              e.preventDefault();
                              const val = (newItemInputs[sec.id] || '').trim();
                              if (!val) return;
                              const nextSecs = [...draft.sections];
                              nextSecs[sIdx] = { ...sec, items: [...sec.items, val] };
                              setDraft({ ...draft, sections: nextSecs });
                              setNewItemInputs({ ...newItemInputs, [sec.id]: '' });
                            }
                          }}
                          placeholder="输入标签回车添加（如：不吃香菜 / 42码）"
                          className="flex-1 px-2.5 py-1 text-xs bg-panel border border-border rounded-input text-ink"
                        />
                        <button
                          type="button"
                          onClick={() => {
                            const val = (newItemInputs[sec.id] || '').trim();
                            if (!val) return;
                            const nextSecs = [...draft.sections];
                            nextSecs[sIdx] = { ...sec, items: [...sec.items, val] };
                            setDraft({ ...draft, sections: nextSecs });
                            setNewItemInputs({ ...newItemInputs, [sec.id]: '' });
                          }}
                          className="px-2.5 py-1 rounded-input bg-overlay text-xs text-ink cursor-pointer"
                        >
                          添加
                        </button>
                      </div>
                    )}
                  </div>
                ))}

                {editing && (
                  <button
                    type="button"
                    onClick={addSection}
                    className="w-full py-2.5 rounded-card border border-dashed border-border-strong text-xs text-muted hover:text-ink flex items-center justify-center gap-1 cursor-pointer"
                  >
                    <Plus className="w-3.5 h-3.5" />
                    <span>添加模块</span>
                  </button>
                )}
              </div>
            </>
          )}
        </div>
      </motion.aside>
    </motion.div>
  );
};
