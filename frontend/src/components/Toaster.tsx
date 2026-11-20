import React, { useEffect, useState } from 'react';
import { AnimatePresence, motion } from 'framer-motion';

type ToastTone = 'info' | 'success' | 'error';

interface ToastItem {
  id: number;
  text: string;
  tone: ToastTone;
}

let seq = 0;

/** Fire-and-forget in-app toast (replaces native alert() which breaks the visual language). */
export function toast(text: string, tone: ToastTone = 'info') {
  window.dispatchEvent(new CustomEvent('kd:toast', { detail: { id: ++seq, text, tone } }));
}

export const Toaster: React.FC = () => {
  const [items, setItems] = useState<ToastItem[]>([]);

  useEffect(() => {
    const onToast = (ev: Event) => {
      const item = (ev as CustomEvent<ToastItem>).detail;
      setItems((prev) => [...prev.slice(-2), item]);
      window.setTimeout(() => {
        setItems((prev) => prev.filter((x) => x.id !== item.id));
      }, 2600);
    };
    window.addEventListener('kd:toast', onToast);
    return () => window.removeEventListener('kd:toast', onToast);
  }, []);

  return (
    <div
      aria-live="polite"
      className="fixed z-[70] left-1/2 -translate-x-1/2 flex flex-col items-center gap-2 pointer-events-none"
      style={{ bottom: 'max(1.25rem, calc(env(safe-area-inset-bottom) + 0.75rem))' }}
    >
      <AnimatePresence>
        {items.map((t) => (
          <motion.div
            key={t.id}
            initial={{ opacity: 0, y: 10, scale: 0.98 }}
            animate={{ opacity: 1, y: 0, scale: 1 }}
            exit={{ opacity: 0, y: 6 }}
            transition={{ duration: 0.2 }}
            className="flex items-center gap-2 px-4 py-2 rounded-chip bg-ink text-surface text-xs shadow-float max-w-[86vw]"
          >
            <span
              className={`w-1.5 h-1.5 rounded-chip shrink-0 ${
                t.tone === 'error' ? 'bg-red-400' : t.tone === 'success' ? 'bg-emerald-400' : 'bg-accent'
              }`}
            />
            <span className="truncate">{t.text}</span>
          </motion.div>
        ))}
      </AnimatePresence>
    </div>
  );
};
