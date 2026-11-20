import { useEffect, useRef } from 'react';

let lockCount = 0;

/**
 * Locks background scrolling while an overlay is open and closes it on Escape.
 * Ref-counted so stacked overlays (e.g. photo opened from the pet drawer) behave correctly.
 */
export function useOverlay(active: boolean, onEscape?: () => void) {
  const escRef = useRef(onEscape);
  escRef.current = onEscape;

  useEffect(() => {
    if (!active) return;

    lockCount += 1;
    if (lockCount === 1) {
      const scrollbarW = Math.max(0, window.innerWidth - document.documentElement.clientWidth);
      document.documentElement.style.setProperty('--kd-sbw', `${scrollbarW}px`);
      document.body.style.overflow = 'hidden';
      if (scrollbarW > 0) {
        document.body.style.paddingRight = `${scrollbarW}px`;
      }
    }

    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') escRef.current?.();
    };
    window.addEventListener('keydown', onKey);

    return () => {
      window.removeEventListener('keydown', onKey);
      lockCount = Math.max(0, lockCount - 1);
      if (lockCount === 0) {
        document.body.style.overflow = '';
        document.body.style.paddingRight = '';
        document.documentElement.style.removeProperty('--kd-sbw');
      }
    };
  }, [active]);
}
