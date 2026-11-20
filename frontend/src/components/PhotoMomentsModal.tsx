import React, { useEffect, useRef, useState } from 'react';
import { AnimatePresence, motion } from 'framer-motion';
import {
  AlertCircle,
  ArrowUp,
  ArrowUpRight,
  ChevronLeft,
  ChevronRight,
  Film,
  Heart,
  Loader2,
  MapPin,
  Mic,
  Play,
  RotateCw,
  Square,
  Trash2,
  Volume2,
  X,
} from 'lucide-react';
import { api } from '../lib/apiClient';
import { ASSET_KIND, resolveMediaUrl, uploadRawMediaBlob } from '../lib/mediaProcessor';
import { useOverlay } from '../hooks/useOverlay';
import { toast } from './Toaster';
import type {
  AssetUrlMap,
  PhotoCardDto,
  PhotoComment,
  SnowflakeId,
  SpaceDto,
  UserMap,
} from '../types/api';

interface PhotoMomentsModalProps {
  spaceId: SnowflakeId;
  selfUserId: SnowflakeId;
  photo: PhotoCardDto;
  photoIndex: number;
  totalPhotos: number;
  spaces: SpaceDto[];
  usersMap: UserMap;
  assetUrls: AssetUrlMap;
  hasPrev: boolean;
  hasNext: boolean;
  onNavigate: (dir: -1 | 1) => void;
  onClose: () => void;
  onPhotoMutated: (updated: PhotoCardDto, extraUrls?: AssetUrlMap) => void;
  onPhotoDeleted: (photoId: SnowflakeId) => void;
}

function relativeTime(ts: number): string {
  const diff = Date.now() - ts;
  const min = Math.floor(diff / 60000);
  if (min < 1) return '刚刚';
  if (min < 60) return `${min} 分钟前`;
  const h = Math.floor(min / 60);
  if (h < 24) return `${h} 小时前`;
  const d = Math.floor(h / 24);
  if (d < 7) return `${d} 天前`;
  return new Date(ts).toLocaleDateString('zh-CN', { month: 'numeric', day: 'numeric' });
}

function formatEditorialDate(ts: number): string {
  const d = new Date(ts);
  const y = d.getFullYear();
  const m = String(d.getMonth() + 1).padStart(2, '0');
  const day = String(d.getDate()).padStart(2, '0');
  return `${y}.${m}.${day}`;
}

export const PhotoMomentsModal: React.FC<PhotoMomentsModalProps> = ({
  spaceId,
  selfUserId,
  photo,
  photoIndex,
  totalPhotos,
  spaces,
  usersMap,
  assetUrls,
  hasPrev,
  hasNext,
  onNavigate,
  onClose,
  onPhotoMutated,
  onPhotoDeleted,
}) => {
  const [commentText, setCommentText] = useState('');
  const [replyToId, setReplyToId] = useState<SnowflakeId | null>(null);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [heartBurst, setHeartBurst] = useState(0);
  const [livePlaying, setLivePlaying] = useState(false);
  const latestPhotoRef = useRef<PhotoCardDto>(photo);
  latestPhotoRef.current = photo;
  const isUploadingPhoto = String(photo.id).startsWith('temp_');

  const [recording, setRecording] = useState(false);
  const [recordSecs, setRecordSecs] = useState(0);
  const [playingCommentId, setPlayingCommentId] = useState<SnowflakeId | null>(null);
  const mediaRecorderRef = useRef<MediaRecorder | null>(null);
  const audioChunksRef = useRef<Blob[]>([]);
  const waveformSamplesRef = useRef<number[]>([]);
  const recordStartRef = useRef<number>(0);
  const recordIntervalRef = useRef<number | null>(null);
  const audioRef = useRef<HTMLAudioElement | null>(null);
  const lastTapRef = useRef<number>(0);
  const touchStartX = useRef<number | null>(null);
  const commentListRef = useRef<HTMLDivElement | null>(null);

  const [projectOpen, setProjectOpen] = useState(false);

  useOverlay(true, () => (recording ? undefined : onClose()));

  const otherSpaces = spaces.filter((s) => s.id !== spaceId);
  const thumbUrl = resolveMediaUrl(photo.asset_id, assetUrls, true);
  const fullImgUrl = resolveMediaUrl(photo.asset_id, assetUrls, false) || thumbUrl;
  const liveUrl = resolveMediaUrl(photo.post.live_asset_id, assetUrls, false);
  const [displaySrc, setDisplaySrc] = useState<string | null>(() => thumbUrl || fullImgUrl || null);
  const aspectRatio =
    photo.post.width && photo.post.height ? photo.post.width / photo.post.height : null;

  const likedByMe = photo.post.likes.includes(selfUserId);
  const uploader = usersMap[photo.post.uploader_id];
  const uploaderName = uploader?.profile.nickname || '空间成员';
  const uploaderAvatar = resolveMediaUrl(uploader?.profile.avatar_asset_id, assetUrls, true);

  // Zero-jitter progressive decode: show cached thumbnail immediately at exact aspect ratio,
  // pre-decode high-res image off-DOM, and swap atomically with 0px layout shift!
  useEffect(() => {
    let cancelled = false;
    setDisplaySrc(thumbUrl || fullImgUrl || null);
    if (!fullImgUrl || fullImgUrl === thumbUrl) return;

    const pre = new Image();
    pre.src = fullImgUrl;
    const commit = () => {
      if (!cancelled) setDisplaySrc(fullImgUrl);
    };
    if (pre.complete && pre.naturalWidth > 0) {
      commit();
    } else if (typeof pre.decode === 'function') {
      pre.decode().then(commit).catch(() => {
        pre.onload = commit;
      });
    } else {
      pre.onload = commit;
    }
    return () => {
      cancelled = true;
    };
  }, [photo.id, thumbUrl, fullImgUrl]);

  // Arrow-key navigation (ignored while typing)
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const tag = (e.target as HTMLElement)?.tagName;
      if (tag === 'INPUT' || tag === 'TEXTAREA') return;
      if (e.key === 'ArrowLeft' && hasPrev) onNavigate(-1);
      if (e.key === 'ArrowRight' && hasNext) onNavigate(1);
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [hasPrev, hasNext, onNavigate]);

  // Reset transient state when switching photos
  useEffect(() => {
    setReplyToId(null);
    setConfirmDelete(false);
    setProjectOpen(false);
    setLivePlaying(false);
    audioRef.current?.pause();
    setPlayingCommentId(null);
  }, [photo.id]);

  // Co-presence: tell partner which photo I'm looking at
  useEffect(() => {
    api.updatePresence(spaceId, 'PHOTO', photo.id).catch(() => {});
  }, [spaceId, photo.id]);
  useEffect(() => () => void api.updatePresence(spaceId, 'IDLE', null).catch(() => {}), [spaceId]);

  const mergeServerPhotoWithLocalPending = (
    serverPhoto: PhotoCardDto,
    completedTempId?: SnowflakeId,
  ): PhotoCardDto => {
    const remainingPending = latestPhotoRef.current.post.comments.filter(
      (c) => String(c.id).startsWith('temp_') && c.id !== completedTempId,
    );
    if (remainingPending.length === 0) return serverPhoto;
    return {
      ...serverPhoto,
      post: {
        ...serverPhoto.post,
        comments: [...serverPhoto.post.comments, ...remainingPending],
      },
    };
  };

  const handleToggleLike = async () => {
    if (isUploadingPhoto) return;
    const prevPhoto = latestPhotoRef.current;
    const wasLiked = prevPhoto.post.likes.includes(selfUserId);
    if (!wasLiked) setHeartBurst((n) => n + 1);

    // 0ms WeChat-style optimistic toggle
    const optimisticLikes = wasLiked
      ? prevPhoto.post.likes.filter((id) => id !== selfUserId)
      : [...prevPhoto.post.likes, selfUserId];
    onPhotoMutated({
      ...prevPhoto,
      post: { ...prevPhoto.post, likes: optimisticLikes },
    });

    try {
      const res = await api.toggleLike(spaceId, photo.id);
      onPhotoMutated(mergeServerPhotoWithLocalPending(res.photo), res.asset_urls);
    } catch {
      onPhotoMutated(prevPhoto);
      toast('点赞失败，请稍后再试', 'error');
    }
  };

  const handleStageTap = () => {
    const now = Date.now();
    if (now - lastTapRef.current < 280) {
      if (!likedByMe) handleToggleLike();
      else setHeartBurst((n) => n + 1);
    }
    lastTapRef.current = now;
  };

  const scrollCommentsToEnd = () =>
    requestAnimationFrame(() => commentListRef.current?.scrollTo({ top: 1e6, behavior: 'smooth' }));

  const dispatchTextCommentAsync = async (tempComment: PhotoComment) => {
    try {
      const res = await api.addComment(spaceId, photo.id, {
        reply_to_comment_id:
          tempComment.reply_to_comment_id && !String(tempComment.reply_to_comment_id).startsWith('temp_')
            ? tempComment.reply_to_comment_id
            : null,
        text: tempComment.text || '',
      });
      onPhotoMutated(
        mergeServerPhotoWithLocalPending(res.photo, tempComment.id),
        res.asset_urls,
      );
    } catch {
      const cur = latestPhotoRef.current;
      onPhotoMutated({
        ...cur,
        post: {
          ...cur.post,
          comments: cur.post.comments.map((c) =>
            c.id === tempComment.id ? { ...c, _transferState: 'failed' } : c,
          ),
        },
      });
    }
  };

  const handleSendTextComment = (e: React.FormEvent) => {
    e.preventDefault();
    const trimmed = commentText.trim();
    if (!trimmed || isUploadingPhoto) return;

    const tempComment: PhotoComment = {
      id: `temp_cmt_${Date.now()}_${Math.random().toString(36).slice(2, 6)}`,
      uid: selfUserId,
      reply_to_comment_id: replyToId,
      text: trimmed,
      waveform: [],
      at: Date.now(),
      _transferState: 'sending',
    };

    // 0ms optimistic clear & insert — never freeze input!
    setCommentText('');
    setReplyToId(null);
    const cur = latestPhotoRef.current;
    onPhotoMutated({
      ...cur,
      post: {
        ...cur.post,
        comments: [...cur.post.comments, tempComment],
      },
    });
    scrollCommentsToEnd();
    void dispatchTextCommentAsync(tempComment);
  };

  const dispatchVoiceCommentAsync = async (
    tempComment: PhotoComment,
    blob: Blob,
    localAudioUrl: string,
  ) => {
    try {
      const audioAssetId = await uploadRawMediaBlob(
        blob,
        ASSET_KIND.VOICE,
        blob.type || 'audio/webm',
      );
      const res = await api.addComment(spaceId, photo.id, {
        reply_to_comment_id:
          tempComment.reply_to_comment_id && !String(tempComment.reply_to_comment_id).startsWith('temp_')
            ? tempComment.reply_to_comment_id
            : null,
        audio_asset_id: audioAssetId,
        duration_sec: tempComment.duration_sec || 3,
        waveform: tempComment.waveform,
      });
      URL.revokeObjectURL(localAudioUrl);
      onPhotoMutated(
        mergeServerPhotoWithLocalPending(res.photo, tempComment.id),
        res.asset_urls,
      );
    } catch {
      const cur = latestPhotoRef.current;
      onPhotoMutated({
        ...cur,
        post: {
          ...cur.post,
          comments: cur.post.comments.map((c) =>
            c.id === tempComment.id ? { ...c, _transferState: 'failed' } : c,
          ),
        },
      });
    }
  };

  const retryFailedComment = (c: PhotoComment) => {
    const cur = latestPhotoRef.current;
    const resetItem: PhotoComment = { ...c, _transferState: 'sending' };
    onPhotoMutated({
      ...cur,
      post: {
        ...cur.post,
        comments: cur.post.comments.map((x) => (x.id === c.id ? resetItem : x)),
      },
    });
    if (c._localAudioBlob && c.audio_asset_id) {
      const localUrl = resolveMediaUrl(c.audio_asset_id, assetUrls, false) || '';
      void dispatchVoiceCommentAsync(resetItem, c._localAudioBlob, localUrl);
    } else {
      void dispatchTextCommentAsync(resetItem);
    }
  };

  const startVoiceRecording = async () => {
    if (isUploadingPhoto) return;
    try {
      const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
      const audioCtx = new AudioContext();
      const analyser = audioCtx.createAnalyser();
      analyser.fftSize = 64;
      audioCtx.createMediaStreamSource(stream).connect(analyser);
      const dataArr = new Uint8Array(analyser.frequencyBinCount);

      const recorder = new MediaRecorder(stream);
      audioChunksRef.current = [];
      waveformSamplesRef.current = [];
      recordStartRef.current = Date.now();
      setRecordSecs(0);
      setRecording(true);

      recorder.ondataavailable = (ev) => {
        if (ev.data.size > 0) audioChunksRef.current.push(ev.data);
      };
      recorder.onstop = () => {
        stream.getTracks().forEach((t) => t.stop());
        audioCtx.close().catch(() => {});
        if (recordIntervalRef.current) window.clearInterval(recordIntervalRef.current);
        setRecording(false);

        const durationSec = Math.max(
          1,
          Math.min(15, Math.round((Date.now() - recordStartRef.current) / 1000)),
        );
        const blob = new Blob(audioChunksRef.current, { type: recorder.mimeType || 'audio/webm' });
        if (blob.size < 200) return;

        const raw = waveformSamplesRef.current;
        const step = Math.max(1, Math.floor(raw.length / 20));
        const waveform = Array.from(
          { length: 20 },
          (_, i) => raw[Math.min(raw.length - 1, i * step)] || 32,
        );

        const tempAudioId = `temp_aud_${Date.now()}`;
        const localAudioUrl = URL.createObjectURL(blob);
        const tempComment: PhotoComment = {
          id: `temp_vcmt_${Date.now()}`,
          uid: selfUserId,
          reply_to_comment_id: replyToId,
          audio_asset_id: tempAudioId,
          duration_sec: durationSec,
          waveform,
          at: Date.now(),
          _transferState: 'sending',
          _localAudioBlob: blob,
        };

        setReplyToId(null);
        const cur = latestPhotoRef.current;
        onPhotoMutated(
          {
            ...cur,
            post: { ...cur.post, comments: [...cur.post.comments, tempComment] },
          },
          { [tempAudioId]: { full_url: localAudioUrl, thumb_url: localAudioUrl } },
        );
        scrollCommentsToEnd();
        void dispatchVoiceCommentAsync(tempComment, blob, localAudioUrl);
      };

      recorder.start();
      mediaRecorderRef.current = recorder;
      recordIntervalRef.current = window.setInterval(() => {
        analyser.getByteFrequencyData(dataArr);
        const avg = dataArr.reduce((a, b) => a + b, 0) / dataArr.length;
        waveformSamplesRef.current.push(
          Math.max(16, Math.min(96, Math.round((avg / 255) * 100))),
        );
        const elapsed = Math.floor((Date.now() - recordStartRef.current) / 1000);
        setRecordSecs(elapsed);
        if (elapsed >= 15 && recorder.state === 'recording') recorder.stop();
      }, 200);
    } catch {
      toast('无法使用麦克风，请在浏览器中允许录音权限', 'error');
    }
  };

  const stopVoiceRecording = () => {
    if (mediaRecorderRef.current?.state === 'recording') mediaRecorderRef.current.stop();
  };

  const playVoiceComment = (commentId: SnowflakeId, audioAssetId?: SnowflakeId | null) => {
    const url = resolveMediaUrl(audioAssetId, assetUrls, false);
    if (!url) return;
    audioRef.current?.pause();
    if (playingCommentId === commentId) {
      setPlayingCommentId(null);
      return;
    }
    const audio = new Audio(url);
    audioRef.current = audio;
    setPlayingCommentId(commentId);
    audio.onended = () => setPlayingCommentId(null);
    audio.play().catch(() => setPlayingCommentId(null));
  };

  const handleProjectToSpace = async (target: SpaceDto) => {
    try {
      await api.projectPhoto(spaceId, photo.id, target.id);
      toast(`已冲洗到「${target.name}」`, 'success');
      setProjectOpen(false);
    } catch (err) {
      toast(err instanceof Error ? err.message : '投递失败', 'error');
    }
  };

  const handleDelete = async () => {
    try {
      await api.deletePhoto(spaceId, photo.id);
      toast('已从照片墙移除');
      onPhotoDeleted(photo.id);
    } catch {
      toast('删除失败', 'error');
    }
  };

  const replyTarget = replyToId ? photo.post.comments.find((c) => c.id === replyToId) : null;
  const hasCaption = Boolean(photo.post.caption?.trim());

  return (
    <motion.div
      initial={{ opacity: 0 }}
      animate={{ opacity: 1 }}
      exit={{ opacity: 0 }}
      transition={{ duration: 0.16, ease: 'easeOut' }}
      onClick={onClose}
      className="fixed inset-0 z-50 bg-black/60 backdrop-blur-sm flex items-end md:items-center justify-center p-0 md:px-8 md:py-7 lg:px-12"
    >
      <motion.div
        initial={{ opacity: 0, scale: 0.985, y: 6 }}
        animate={{ opacity: 1, scale: 1, y: 0 }}
        exit={{ opacity: 0, scale: 0.985, y: 6 }}
        transition={{ duration: 0.18, ease: [0.22, 1, 0.36, 1] }}
        onClick={(e) => e.stopPropagation()}
        className="w-full md:max-w-[1160px] h-[94dvh] md:h-[86dvh] md:max-h-[840px] bg-panel border border-border rounded-t-[16px] md:rounded-[16px] overflow-hidden flex flex-col md:flex-row shadow-[0_32px_88px_rgba(0,0,0,0.34)] transform-gpu"
      >
        {/* =================================================================
            LEFT EXHIBITION STAGE (72% Golden Gallery Passe-Partout Mat)
            Cohesive archival board in Day & Night — zero muddy blur!
           ================================================================= */}
        <div
          onClick={handleStageTap}
          onTouchStart={(e) => (touchStartX.current = e.touches[0].clientX)}
          onTouchEnd={(e) => {
            if (touchStartX.current === null) return;
            const dx = e.changedTouches[0].clientX - touchStartX.current;
            touchStartX.current = null;
            if (dx > 60 && hasPrev) onNavigate(-1);
            if (dx < -60 && hasNext) onNavigate(1);
          }}
          style={{ backgroundColor: 'var(--kd-mat)' }}
          className="group/stage relative w-full md:flex-1 h-[48dvh] md:h-full p-5 sm:p-8 md:p-10 lg:p-12 flex items-center justify-center overflow-hidden shrink-0 select-none"
        >
          {/* Container-size locked print frame: 0.00px jump across thumb -> full decode */}
          <div
            style={{ containerType: 'size' }}
            className="relative w-full h-full flex items-center justify-center"
          >
            {displaySrc && (
              <div
                style={
                  aspectRatio
                    ? {
                        width: `min(100cqw, calc(100cqh * ${aspectRatio}))`,
                        height: `min(100cqh, calc(100cqw / ${aspectRatio}))`,
                        boxShadow: 'var(--kd-print-shadow)',
                      }
                    : { boxShadow: 'var(--kd-print-shadow)' }
                }
                className={`relative rounded-[2px] ring-1 ring-border overflow-hidden ${
                  aspectRatio ? '' : 'max-w-full max-h-full'
                }`}
              >
                <img
                  key={photo.id}
                  src={displaySrc}
                  alt={photo.post.caption}
                  draggable={false}
                  className={`block ${
                    aspectRatio ? 'w-full h-full object-cover' : 'max-w-full max-h-full object-contain'
                  }`}
                />

                {liveUrl && livePlaying && (
                  <video
                    src={liveUrl}
                    autoPlay
                    muted
                    playsInline
                    loop
                    className="absolute inset-0 w-full h-full object-cover"
                  />
                )}

                {liveUrl && (
                  <button
                    type="button"
                    onMouseEnter={() => setLivePlaying(true)}
                    onMouseLeave={() => setLivePlaying(false)}
                    onClick={(e) => {
                      e.stopPropagation();
                      setLivePlaying((v) => !v);
                    }}
                    className="absolute top-2.5 right-2.5 px-2 py-0.5 rounded-chip bg-black/40 backdrop-blur-md text-[9px] tracking-wider text-white/95 flex items-center gap-1 cursor-pointer"
                  >
                    <Film className="w-2.5 h-2.5" />
                    <span>LIVE</span>
                  </button>
                )}
              </div>
            )}
          </div>

          {/* Subtle Prev / Next Gallery Controls Inside Mat */}
          {hasPrev && (
            <button
              type="button"
              aria-label="上一张"
              onClick={(e) => {
                e.stopPropagation();
                onNavigate(-1);
              }}
              className="hidden md:flex absolute left-2.5 lg:left-3 top-1/2 -translate-y-1/2 w-8 h-8 rounded-chip bg-panel/85 hover:bg-panel border border-border text-ink shadow-xs items-center justify-center opacity-0 group-hover/stage:opacity-100 transition-opacity duration-150 cursor-pointer"
            >
              <ChevronLeft className="w-4 h-4" />
            </button>
          )}
          {hasNext && (
            <button
              type="button"
              aria-label="下一张"
              onClick={(e) => {
                e.stopPropagation();
                onNavigate(1);
              }}
              className="hidden md:flex absolute right-2.5 lg:right-3 top-1/2 -translate-y-1/2 w-8 h-8 rounded-chip bg-panel/85 hover:bg-panel border border-border text-ink shadow-xs items-center justify-center opacity-0 group-hover/stage:opacity-100 transition-opacity duration-150 cursor-pointer"
            >
              <ChevronRight className="w-4 h-4" />
            </button>
          )}

          {/* Quiet Gallery Plate Index (Only when space has > 1 photo) */}
          {totalPhotos > 1 && (
            <div className="hidden md:block absolute bottom-3.5 left-5 text-[10px] tracking-[0.18em] text-subtle tabular-nums pointer-events-none">
              {String(photoIndex + 1).padStart(2, '0')} / {String(totalPhotos).padStart(2, '0')}
            </div>
          )}

          {/* Double-tap heart burst */}
          <AnimatePresence>
            {heartBurst > 0 && (
              <motion.div
                key={heartBurst}
                initial={{ opacity: 0, scale: 0.6 }}
                animate={{ opacity: [0, 1, 0], scale: [0.6, 1.08, 1] }}
                transition={{ duration: 0.7 }}
                onAnimationComplete={() => setHeartBurst(0)}
                className="absolute inset-0 flex items-center justify-center pointer-events-none"
              >
                <Heart className="w-16 h-16 text-white fill-white drop-shadow-lg" />
              </motion.div>
            )}
          </AnimatePresence>

          {/* Mobile Close Button */}
          <button
            type="button"
            onClick={(e) => {
              e.stopPropagation();
              onClose();
            }}
            className="md:hidden absolute top-3 right-3 w-8 h-8 rounded-chip bg-panel/85 backdrop-blur border border-border text-ink flex items-center justify-center"
            style={{ top: 'max(0.75rem, env(safe-area-inset-top))' }}
          >
            <X className="w-4 h-4" />
          </button>
        </div>

        {/* =================================================================
            RIGHT EDITORIAL COLUMN (~28% Proportion — Intimate, Balanced)
           ================================================================= */}
        <div className="w-full md:w-[320px] lg:w-[340px] shrink-0 flex-1 min-h-0 md:flex-initial md:h-full flex flex-col bg-panel text-ink border-t md:border-t-0 md:border-l border-border">
          {/* Author & Date Header */}
          <div className="px-5 py-4 border-b border-border flex items-center justify-between gap-3 shrink-0">
            <div className="flex items-center gap-2.5 min-w-0">
              <div className="w-8 h-8 rounded-chip bg-overlay border border-border flex items-center justify-center overflow-hidden shrink-0">
                {uploaderAvatar ? (
                  <img
                    src={uploaderAvatar}
                    alt={uploaderName}
                    className="w-full h-full object-cover"
                  />
                ) : (
                  <span className="text-xs font-medium text-ink">
                    {uploaderName.slice(0, 1).toUpperCase()}
                  </span>
                )}
              </div>

              <div className="min-w-0">
                <div className="text-[13px] font-medium text-ink truncate leading-tight">
                  {uploaderName}
                </div>
                <div className="flex items-center gap-1.5 text-[11px] text-muted mt-0.5 tabular-nums">
                  <span>{formatEditorialDate(photo.taken_at)}</span>
                  {photo.post.exif?.location && (
                    <>
                      <span className="text-subtle">·</span>
                      <span className="inline-flex items-center gap-0.5 truncate">
                        <MapPin className="w-2.5 h-2.5 shrink-0" />
                        <span className="truncate">{photo.post.exif.location}</span>
                      </span>
                    </>
                  )}
                </div>
              </div>
            </div>

            <div className="flex items-center gap-0.5 shrink-0">
              {otherSpaces.length > 0 && (
                <button
                  type="button"
                  onClick={() => {
                    setConfirmDelete(false);
                    setProjectOpen((v) => !v);
                  }}
                  title="冲洗到其他空间"
                  className={`w-7 h-7 rounded-input flex items-center justify-center transition-colors cursor-pointer ${
                    projectOpen ? 'text-ink bg-overlay' : 'text-muted hover:text-ink hover:bg-overlay'
                  }`}
                >
                  <ArrowUpRight className="w-3.5 h-3.5" />
                </button>
              )}
              <button
                type="button"
                onClick={() => {
                  setProjectOpen(false);
                  setConfirmDelete((v) => !v);
                }}
                title="移除照片"
                className={`w-7 h-7 rounded-input flex items-center justify-center transition-colors cursor-pointer ${
                  confirmDelete
                    ? 'text-accent bg-accent-soft'
                    : 'text-muted hover:text-accent hover:bg-overlay'
                }`}
              >
                <Trash2 className="w-3.5 h-3.5" />
              </button>
              <button
                type="button"
                onClick={onClose}
                aria-label="关闭"
                className="hidden md:flex w-7 h-7 rounded-input items-center justify-center text-muted hover:text-ink hover:bg-overlay transition-colors cursor-pointer"
              >
                <X className="w-4 h-4" />
              </button>
            </div>
          </div>

          {confirmDelete && (
            <div className="border-b border-border bg-accent-soft px-4 py-2.5 flex items-center justify-between gap-3 text-xs shrink-0">
              <span className="text-ink">从当前空间移除此照片？</span>
              <div className="flex gap-1.5 shrink-0">
                <button
                  type="button"
                  onClick={() => setConfirmDelete(false)}
                  className="px-2.5 py-1 rounded-input text-muted hover:text-ink cursor-pointer"
                >
                  取消
                </button>
                <button
                  type="button"
                  onClick={handleDelete}
                  className="px-2.5 py-1 rounded-input bg-accent text-white cursor-pointer"
                >
                  移除
                </button>
              </div>
            </div>
          )}

          {projectOpen && (
            <div className="border-b border-border bg-overlay px-4 py-2.5 space-y-1.5 shrink-0">
              <div className="text-[11px] text-muted">冲洗到其他空间</div>
              <div className="flex flex-wrap gap-1.5">
                {otherSpaces.map((sp) => (
                  <button
                    key={sp.id}
                    type="button"
                    onClick={() => handleProjectToSpace(sp)}
                    className="px-2.5 py-1 rounded-chip bg-panel border border-border text-xs text-ink hover:border-border-strong cursor-pointer"
                  >
                    ↗ {sp.name}
                  </button>
                ))}
              </div>
            </div>
          )}

          {/* Optional Editorial Wall Label (Only rendered when caption exists) */}
          {hasCaption && (
            <div className="px-5 py-3.5 border-b border-border shrink-0">
              <p className="text-[13px] text-ink leading-relaxed whitespace-pre-wrap">
                {photo.post.caption}
              </p>
            </div>
          )}

          {/* Comments Stream */}
          <div ref={commentListRef} className="flex-1 min-h-0 overflow-y-auto px-4 py-3.5">
            {photo.post.comments.length === 0 ? (
              <div className="h-full flex items-center justify-center select-none">
                <span className="text-xs text-subtle tracking-wider">尚无批注</span>
              </div>
            ) : (
              <div className="space-y-1.5">
                {photo.post.comments.map((c) => {
                  const cUser = usersMap[c.uid];
                  const author = cUser?.profile.nickname || '成员';
                  const cAvatar = resolveMediaUrl(cUser?.profile.avatar_asset_id, assetUrls, true);
                  const replied = c.reply_to_comment_id
                    ? photo.post.comments.find((x) => x.id === c.reply_to_comment_id)
                    : null;
                  const repliedAuthor = replied
                    ? usersMap[replied.uid]?.profile.nickname || '成员'
                    : null;
                  const isActiveReply = replyToId === c.id;

                  return (
                    <div
                      key={c.id}
                      onClick={() => setReplyToId(isActiveReply ? null : c.id)}
                      className={`group px-2.5 py-2 rounded-input transition-colors cursor-pointer text-xs flex items-start gap-2.5 ${
                        isActiveReply ? 'bg-accent-soft' : 'hover:bg-overlay/75'
                      }`}
                    >
                      <div className="w-6 h-6 rounded-chip bg-overlay border border-border flex items-center justify-center overflow-hidden shrink-0 mt-0.5">
                        {cAvatar ? (
                          <img src={cAvatar} alt={author} className="w-full h-full object-cover" />
                        ) : (
                          <span className="text-[10px] font-medium text-muted">
                            {author.slice(0, 1).toUpperCase()}
                          </span>
                        )}
                      </div>

                      <div className="flex-1 min-w-0">
                        <div className="flex items-baseline justify-between gap-2 mb-0.5">
                          <div className="truncate">
                            <span className="font-medium text-ink">{author}</span>
                            {repliedAuthor && (
                              <span className="text-muted">
                                {' '}
                                回复 <span className="text-ink">{repliedAuthor}</span>
                              </span>
                            )}
                          </div>
                          <div className="flex items-center gap-1.5 shrink-0">
                            {c._transferState === 'sending' && (
                              <span
                                title="正在异步传输…"
                                className="inline-flex items-center gap-1 text-[10px] text-muted"
                              >
                                <Loader2 className="w-2.5 h-2.5 animate-spin" />
                              </span>
                            )}
                            {c._transferState === 'failed' && (
                              <button
                                type="button"
                                title="发送失败，点击重发"
                                onClick={(e) => {
                                  e.stopPropagation();
                                  retryFailedComment(c);
                                }}
                                className="inline-flex items-center gap-0.5 px-1.5 py-0.5 rounded-chip bg-accent/15 text-accent text-[10px] font-medium cursor-pointer"
                              >
                                <AlertCircle className="w-2.5 h-2.5" />
                                <span>重发</span>
                                <RotateCw className="w-2.5 h-2.5" />
                              </button>
                            )}
                            {c.uid === selfUserId && !c._transferState && (
                              <button
                                type="button"
                                title="删除留言"
                                onClick={(e) => {
                                  e.stopPropagation();
                                  const prev = latestPhotoRef.current;
                                  onPhotoMutated({
                                    ...prev,
                                    post: {
                                      ...prev.post,
                                      comments: prev.post.comments.filter((x) => x.id !== c.id),
                                    },
                                  });
                                  api
                                    .deleteComment(spaceId, photo.id, c.id)
                                    .then((res) =>
                                      onPhotoMutated(
                                        mergeServerPhotoWithLocalPending(res.photo),
                                        res.asset_urls,
                                      ),
                                    )
                                    .catch(() => {
                                      onPhotoMutated(prev);
                                      toast('删除留言失败', 'error');
                                    });
                                }}
                                className="text-subtle hover:text-accent opacity-0 group-hover:opacity-100 transition-opacity cursor-pointer"
                              >
                                <Trash2 className="w-3 h-3" />
                              </button>
                            )}
                            <span className="text-[10px] text-subtle tabular-nums">
                              {relativeTime(c.at)}
                            </span>
                          </div>
                        </div>

                        {c.audio_asset_id ? (
                          <button
                            type="button"
                            onClick={(e) => {
                              e.stopPropagation();
                              playVoiceComment(c.id, c.audio_asset_id);
                            }}
                            className="mt-1 inline-flex items-center gap-2 px-3 py-1.5 rounded-chip bg-panel border border-border hover:border-border-strong transition-colors cursor-pointer"
                          >
                            {playingCommentId === c.id ? (
                              <Volume2 className="w-3.5 h-3.5 text-accent animate-pulse" />
                            ) : (
                              <Play className="w-3 h-3 text-ink fill-current" />
                            )}
                            <span className="flex items-center gap-[2px] h-3.5">
                              {(c.waveform.length ? c.waveform : [25, 60, 80, 40, 70, 50, 30]).map(
                                (amp, idx) => (
                                  <span
                                    key={idx}
                                    style={{ height: `${Math.max(20, amp)}%` }}
                                    className={`w-[2px] rounded-full ${
                                      playingCommentId === c.id ? 'bg-accent' : 'bg-ink/70'
                                    }`}
                                  />
                                ),
                              )}
                            </span>
                            <span className="text-[11px] text-muted tabular-nums">
                              {c.duration_sec || 3}″
                            </span>
                          </button>
                        ) : (
                          <p className="text-ink/90 leading-relaxed whitespace-pre-wrap">{c.text}</p>
                        )}
                      </div>
                    </div>
                  );
                })}
              </div>
            )}
          </div>

          {/* Unified Bottom Interaction & Note Bar */}
          <form
            onSubmit={handleSendTextComment}
            className="px-4 py-3 border-t border-border bg-panel pb-safe space-y-2 shrink-0"
          >
            {photo.post.likes.length > 0 && (
              <div className="flex items-center gap-1.5 text-[11px] text-muted px-0.5 truncate">
                <Heart className="w-3 h-3 text-accent fill-current shrink-0" />
                <span className="truncate">
                  {photo.post.likes
                    .map((uid) => usersMap[uid]?.profile.nickname || '伙伴')
                    .join('、')}
                </span>
              </div>
            )}

            {replyTarget && (
              <div className="flex items-center justify-between text-[11px] text-muted px-1">
                <span className="truncate">
                  回复 {usersMap[replyTarget.uid]?.profile.nickname || '成员'}
                </span>
                <button
                  type="button"
                  onClick={() => setReplyToId(null)}
                  className="text-accent shrink-0 cursor-pointer"
                >
                  取消
                </button>
              </div>
            )}

            {recording ? (
              <button
                type="button"
                onClick={stopVoiceRecording}
                className="w-full flex items-center justify-center gap-2 h-9 px-3 rounded-chip bg-accent text-white text-xs font-medium cursor-pointer"
              >
                <Square className="w-3 h-3 fill-current" />
                <span className="tabular-nums">录音中 {recordSecs}s / 15s · 点击发送</span>
              </button>
            ) : (
              <div className="flex items-center gap-1.5">
                <button
                  type="button"
                  onClick={handleToggleLike}
                  aria-label="点赞"
                  className={`h-9 px-2.5 rounded-chip border inline-flex items-center justify-center gap-1 text-xs transition-[color,background-color,border-color,transform] duration-150 active:scale-95 cursor-pointer shrink-0 tabular-nums ${
                    likedByMe
                      ? 'bg-accent-soft border-accent/30 text-accent font-medium'
                      : 'bg-surface border-border text-muted hover:text-ink hover:border-border-strong'
                  }`}
                >
                  <Heart className={`w-3.5 h-3.5 ${likedByMe ? 'fill-current' : ''}`} />
                  {photo.post.likes.length > 0 && <span>{photo.post.likes.length}</span>}
                </button>

                <button
                  type="button"
                  onClick={startVoiceRecording}
                  disabled={isUploadingPhoto}
                  title="录制语音批注"
                  aria-label="录制语音批注"
                  className="w-9 h-9 rounded-chip bg-surface border border-border flex items-center justify-center text-muted hover:text-ink hover:border-border-strong transition-colors cursor-pointer shrink-0 disabled:opacity-40"
                >
                  <Mic className="w-3.5 h-3.5" />
                </button>

                <div className="flex-1 min-w-0 h-9 rounded-chip bg-surface border border-border focus-within:border-border-strong flex items-center pl-3.5 pr-1 transition-colors">
                  <input
                    type="text"
                    value={commentText}
                    onChange={(e) => setCommentText(e.target.value)}
                    placeholder={
                      isUploadingPhoto
                        ? '照片正在传输至云端，稍候即可留言…'
                        : replyTarget
                          ? '写下回复…'
                          : '写句批注…'
                    }
                    disabled={isUploadingPhoto}
                    enterKeyHint="send"
                    className="flex-1 min-w-0 bg-transparent text-xs text-ink placeholder:text-subtle focus:outline-none disabled:opacity-50"
                  />
                  <button
                    type="submit"
                    disabled={!commentText.trim() || isUploadingPhoto}
                    aria-label="发送"
                    className="w-7 h-7 rounded-chip bg-ink text-surface flex items-center justify-center disabled:opacity-20 transition-opacity cursor-pointer shrink-0"
                  >
                    <ArrowUp className="w-3.5 h-3.5" />
                  </button>
                </div>
              </div>
            )}
          </form>
        </div>
      </motion.div>
    </motion.div>
  );
};
