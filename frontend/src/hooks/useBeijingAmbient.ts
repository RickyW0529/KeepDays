import { useCallback, useEffect, useState } from 'react';
import type { AmbientPhase } from '../types/api';

export type ThemePreference = 'auto' | 'daylight' | 'night';

const THEME_PREF_KEY = 'kd_theme_pref';

export interface LocalBeijingAmbient {
  phase: AmbientPhase;
  effectivePhase: AmbientPhase;
  preference: ThemePreference;
  isNight: boolean;
  timeStr: string;
  cyclePreference: () => void;
}

function computeBeijingClock(): { phase: AmbientPhase; timeStr: string } {
  const fmt = new Intl.DateTimeFormat('en-GB', {
    timeZone: 'Asia/Shanghai',
    hour: '2-digit',
    minute: '2-digit',
    hour12: false,
  });
  const parts = fmt.formatToParts(new Date());
  const h = Number(parts.find((p) => p.type === 'hour')?.value ?? '12');
  const m = Number(parts.find((p) => p.type === 'minute')?.value ?? '0');
  const totalMin = h * 60 + m;

  let phase: AmbientPhase = 'NIGHT';
  if (totalMin >= 7 * 60 && totalMin < 17 * 60 + 30) {
    phase = 'DAYLIGHT';
  } else if (totalMin >= 17 * 60 + 30 && totalMin < 19 * 60 + 30) {
    phase = 'DUSK';
  }

  return {
    phase,
    timeStr: `${String(h).padStart(2, '0')}:${String(m).padStart(2, '0')}`,
  };
}

export function useBeijingAmbient(isSharedSpace: boolean): LocalBeijingAmbient {
  const [preference, setPreference] = useState<ThemePreference>(() => {
    const saved = localStorage.getItem(THEME_PREF_KEY);
    return saved === 'daylight' || saved === 'night' ? saved : 'auto';
  });
  const [clock, setClock] = useState(() => computeBeijingClock());

  const effectivePhase: AmbientPhase =
    preference === 'daylight'
      ? 'DAYLIGHT'
      : preference === 'night'
        ? 'NIGHT'
        : clock.phase;

  useEffect(() => {
    const tick = () => setClock(computeBeijingClock());
    const timer = window.setInterval(tick, 30_000);
    return () => window.clearInterval(timer);
  }, []);

  useEffect(() => {
    document.documentElement.dataset.ambient = effectivePhase.toLowerCase();
    const bg =
      effectivePhase === 'NIGHT'
        ? '#161514'
        : effectivePhase === 'DUSK'
          ? '#F1EAE0'
          : '#F6F4EF';
    document.querySelector('meta[name="theme-color"]')?.setAttribute('content', bg);
  }, [effectivePhase]);

  useEffect(() => {
    document.documentElement.dataset.space = isSharedSpace ? 'shared' : 'personal';
  }, [isSharedSpace]);

  const cyclePreference = useCallback(() => {
    setPreference((prev) => {
      const next: ThemePreference =
        prev === 'auto' ? 'daylight' : prev === 'daylight' ? 'night' : 'auto';
      localStorage.setItem(THEME_PREF_KEY, next);
      return next;
    });
  }, []);

  return {
    phase: clock.phase,
    effectivePhase,
    preference,
    isNight: effectivePhase === 'NIGHT',
    timeStr: clock.timeStr,
    cyclePreference,
  };
}
