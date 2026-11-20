import { api } from './apiClient';
import type { AssetUrlMap, SnowflakeId } from '../types/api';

// Snowflake Dimension Asset Kind IDs (Domain 40)
export const ASSET_KIND = {
  PHOTO: 4001,
  LIVE_MP4: 4002,
  VOICE: 4003,
  AVATAR: 4004,
  BANNER: 4005,
  DISH_IMG: 4006,
} as const;

export function resolveMediaUrl(
  assetId: SnowflakeId | null | undefined,
  assetUrls: AssetUrlMap,
  preferThumb = false,
): string | undefined {
  if (!assetId) return undefined;
  const remote = assetUrls[assetId];
  if (!remote) return undefined;
  return (preferThumb && remote.thumb_url) || remote.full_url;
}

/**
 * Browser-side Zero-Server-CPU WebP Image Compressor + 480px Thumbnail Generator:
 * Keeps 1vCPU / 512MB server at 0% CPU!
 */
export async function compressAndUploadImage(
  file: File,
  kindId: number = ASSET_KIND.PHOTO,
  generateThumb = true,
): Promise<{
  assetId: SnowflakeId;
  width: number;
  height: number;
  takenAt: number;
}> {
  const bitmap = await createImageBitmap(file);
  const origW = bitmap.width;
  const origH = bitmap.height;

  const maxLong = 2400;
  const scale = Math.min(1, maxLong / Math.max(origW, origH));
  const fullW = Math.max(1, Math.round(origW * scale));
  const fullH = Math.max(1, Math.round(origH * scale));

  const fullBlob = await renderToWebpBlob(bitmap, fullW, fullH, 0.86);
  let thumbBlob: Blob | null = null;
  if (generateThumb) {
    const tScale = Math.min(1, 480 / Math.max(origW, origH));
    thumbBlob = await renderToWebpBlob(
      bitmap,
      Math.max(1, Math.round(origW * tScale)),
      Math.max(1, Math.round(origH * tScale)),
      0.78,
    );
  }
  bitmap.close();

  const presign = await api.presignUpload(
    kindId,
    'image/webp',
    fullBlob.size,
    Boolean(thumbBlob),
  );

  const putMain = await fetch(presign.put_url, {
    method: 'PUT',
    headers: { 'Content-Type': 'image/webp' },
    body: fullBlob,
  });
  if (!putMain.ok) {
    throw new Error(`图片上传存储失败 (${putMain.status})`);
  }

  if (thumbBlob && presign.thumb_put_url) {
    const putThumb = await fetch(presign.thumb_put_url, {
      method: 'PUT',
      headers: { 'Content-Type': 'image/webp' },
      body: thumbBlob,
    });
    if (!putThumb.ok) {
      throw new Error(`缩略图上传失败 (${putThumb.status})`);
    }
  }

  await api.confirmUpload(presign.asset_id);

  return {
    assetId: presign.asset_id,
    width: fullW,
    height: fullH,
    takenAt: file.lastModified || Date.now(),
  };
}

export async function uploadRawMediaBlob(
  blob: Blob,
  kindId: number,
  mimeType: string,
): Promise<SnowflakeId> {
  // Strip `;codecs=...` suffix (e.g. `audio/webm;codecs=opus` -> `audio/webm`)
  const cleanMime = (mimeType.split(';')[0] || 'application/octet-stream')
    .trim()
    .toLowerCase();

  const presign = await api.presignUpload(kindId, cleanMime, blob.size, false);
  const res = await fetch(presign.put_url, {
    method: 'PUT',
    headers: { 'Content-Type': cleanMime },
    body: blob,
  });
  if (!res.ok) {
    throw new Error(`媒体上传失败 (${res.status})`);
  }
  await api.confirmUpload(presign.asset_id);
  return presign.asset_id;
}

async function renderToWebpBlob(
  bitmap: ImageBitmap,
  w: number,
  h: number,
  quality: number,
): Promise<Blob> {
  const canvas = document.createElement('canvas');
  canvas.width = w;
  canvas.height = h;
  const ctx = canvas.getContext('2d');
  if (!ctx) throw new Error('Canvas 2D context unavailable');
  ctx.drawImage(bitmap, 0, 0, w, h);

  return new Promise<Blob>((resolve, reject) => {
    canvas.toBlob(
      (b) => (b ? resolve(b) : reject(new Error('WebP encoding failed'))),
      'image/webp',
      quality,
    );
  });
}

export async function predecodeImageOffscreen(url?: string): Promise<void> {
  if (!url) return;
  try {
    const img = new Image();
    img.src = url;
    await img.decode();
  } catch {
    // Non-blocking best-effort pre-decode
  }
}
