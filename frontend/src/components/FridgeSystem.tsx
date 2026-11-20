import React, { useEffect, useRef, useState } from 'react';
import { AnimatePresence, motion } from 'framer-motion';
import {
  AlertCircle,
  Archive,
  ArrowUpRight,
  Check,
  CheckSquare,
  ChevronDown,
  Clock,
  Loader2,
  Lock,
  PawPrint,
  Pin,
  Plus,
  RotateCcw,
  RotateCw,
  ShoppingBag,
  Square,
  Trash2,
  X,
} from 'lucide-react';
import { api } from '../lib/apiClient';
import {
  ASSET_KIND,
  compressAndUploadImage,
  resolveMediaUrl,
} from '../lib/mediaProcessor';
import { useOverlay } from '../hooks/useOverlay';
import { toast } from './Toaster';
import type {
  BoardNote,
  BoardTodo,
  FridgeBundleResp,
  MenuDish,
  SnowflakeId,
  SpaceDto,
} from '../types/api';

export type FridgeStage = 'collapsed' | 'door' | 'split_board' | 'split_menu';

interface FridgeSystemProps {
  space: SpaceDto;
  allSpaces: SpaceDto[];
  selfUserId: SnowflakeId;
  stage: FridgeStage;
  isNightBeijing: boolean;
  partnerInFridge: boolean;
  canPetPickMenu: boolean;
  bundle: FridgeBundleResp | null;
  fridgeAnchorRef: React.RefObject<HTMLButtonElement | null>;
  onOpenFridge: () => void;
  onChangeStage: (next: FridgeStage) => void;
  onBundleChange: (next: FridgeBundleResp) => void;
}

const PAPER_BG_CLASS: Record<string, string> = {
  cotton_white: 'bg-paper-white',
  oat: 'bg-paper-oat',
  sage: 'bg-paper-sage',
  blush: 'bg-paper-blush',
};

export const FridgeSystem: React.FC<FridgeSystemProps> = ({
  space,
  allSpaces,
  selfUserId,
  stage,
  isNightBeijing,
  partnerInFridge,
  canPetPickMenu,
  bundle,
  fridgeAnchorRef,
  onOpenFridge,
  onChangeStage,
  onBundleChange,
}) => {
  const [activeCat, setActiveCat] = useState<string>('全部');
  const [checkedDishIds, setCheckedDishIds] = useState<SnowflakeId[]>([]);
  const [orderNote, setOrderNote] = useState('');
  const [pawCommentary, setPawCommentary] = useState<string | null>(null);

  const [addingDish, setAddingDish] = useState(false);
  const [dishName, setDishName] = useState('');
  const [dishCat, setDishCat] = useState('荤菜');
  const [dishIngredients, setDishIngredients] = useState('');
  const [dishNote, setDishNote] = useState('');
  const [dishFile, setDishFile] = useState<File | null>(null);

  const [noteText, setNoteText] = useState('');
  const [noteColor, setNoteColor] = useState<'oat' | 'cotton_white' | 'sage' | 'blush'>('oat');
  const [noteUnlockTime, setNoteUnlockTime] = useState('');
  const [todoText, setTodoText] = useState('');
  const [todoCat, setTodoCat] = useState('日常');
  const [showArchivedTodos, setShowArchivedTodos] = useState(false);
  const [busy, setBusy] = useState(false);
  const latestBundleRef = useRef<FridgeBundleResp | null>(bundle);
  latestBundleRef.current = bundle;
  const boardSyncChainRef = useRef<Promise<void>>(Promise.resolve());

  useOverlay(stage !== 'collapsed', () => onChangeStage('collapsed'));

  useEffect(() => {
    if (stage !== 'collapsed') {
      api.updatePresence(space.id, 'FRIDGE', null).catch(() => {});
      return () => {
        api.updatePresence(space.id, 'IDLE', null).catch(() => {});
      };
    }
  }, [space.id, stage]);

  const slips = bundle?.fridge.data.recent_slips || [];
  const menu = bundle?.menu.data || {
    categories: ['荤菜', '素菜', '汤羹', '主食', '甜饮'],
    dishes: [],
  };
  const board = bundle?.board.data || { notes: [], todos: [] };
  const users = bundle?.users || {};
  const assetUrls = bundle?.asset_urls || {};
  const otherSharedSpaces = allSpaces.filter((s) => s.id !== space.id && s.is_shared);

  const openTodoCount = board.todos.filter((t) => !t.done).length;
  const previewNote = board.notes.find((n) => !n.locked && n.text)?.text;

  const toggleCheckDish = (id: SnowflakeId) => {
    setCheckedDishIds((prev) =>
      prev.includes(id) ? prev.filter((x) => x !== id) : [...prev, id],
    );
  };

  const handleConfirmTodayMenu = async () => {
    if (checkedDishIds.length === 0 || busy) return;
    setBusy(true);
    try {
      const updated = await api.pinMenuSlip(space.id, checkedDishIds, orderNote);
      onBundleChange(updated);
      setCheckedDishIds([]);
      setOrderNote('');
      setPawCommentary(null);
      toast('已贴上冰箱门', 'success');
    } catch (err) {
      toast(err instanceof Error ? err.message : '操作失败', 'error');
    } finally {
      setBusy(false);
    }
  };

  const handlePawPick = async () => {
    if (busy) return;
    setBusy(true);
    try {
      const res = await api.pawPickMenu(space.id);
      setCheckedDishIds(res.picked_dish_ids);
      setPawCommentary(res.pet_commentary);
    } catch (err) {
      toast(err instanceof Error ? err.message : '暂时无法选菜', 'info');
    } finally {
      setBusy(false);
    }
  };

  const refreshOnConflict = async (err: unknown) => {
    const msg = err instanceof Error ? err.message : '操作失败';
    toast(msg, 'error');
    if (msg.includes('更新过') || msg.includes('409')) {
      api
        .getFridgeBundle(space.id)
        .then((latest) => onBundleChange(latest))
        .catch(() => {});
    }
  };

  const handleCreateDish = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!bundle || !dishName.trim() || busy) return;
    setBusy(true);
    try {
      let coverId: SnowflakeId | undefined;
      if (dishFile) {
        const up = await compressAndUploadImage(dishFile, ASSET_KIND.DISH_IMG, true);
        coverId = up.assetId;
      }
      const nextDishes = [
        ...menu.dishes,
        {
          name: dishName.trim(),
          category: dishCat,
          cover_asset_id: coverId,
          ingredients: dishIngredients
            .split(/[,，、\s]+/)
            .map((s) => s.trim())
            .filter(Boolean),
          note: dishNote.trim(),
        },
      ];
      const updated = await api.updateMenu(
        space.id,
        bundle.menu.version,
        menu.categories,
        nextDishes,
      );
      onBundleChange(updated);
      setDishName('');
      setDishIngredients('');
      setDishNote('');
      setDishFile(null);
      setAddingDish(false);
    } catch (err) {
      await refreshOnConflict(err);
    } finally {
      setBusy(false);
    }
  };

  const handleDeleteDish = async (dishId: SnowflakeId) => {
    if (!bundle) return;
    try {
      const next = menu.dishes.filter((d) => d.id !== dishId);
      const updated = await api.updateMenu(space.id, bundle.menu.version, menu.categories, next);
      onBundleChange(updated);
    } catch (err) {
      await refreshOnConflict(err);
    }
  };

  /**
   * Serialized, non-blocking optimistic board mutation queue:
   * 1. Applies optimistic state in 0ms (`_transferState: 'sending'`).
   * 2. Sends `PUT /fridge/board` sequentially in the background using the freshest `board.version`.
   * 3. If a partner edited simultaneously (`409`), auto-fetches latest bundle and retries once!
   */
  const enqueueBoardMutation = (
    computeTarget: (base: { notes: BoardNote[]; todos: BoardTodo[] }) => {
      notes: BoardNote[];
      todos: BoardTodo[];
    },
    tempIdsToMarkFailed: SnowflakeId[] = [],
  ) => {
    const cur = latestBundleRef.current;
    if (!cur) return;

    const optimistic = computeTarget(cur.board.data);
    const optimisticBundle: FridgeBundleResp = {
      ...cur,
      board: {
        ...cur.board,
        data: optimistic,
      },
    };
    latestBundleRef.current = optimisticBundle;
    onBundleChange(optimisticBundle);

    const serializeNotes = (list: BoardNote[]) =>
      list.map((n) => ({
        id: String(n.id).startsWith('temp_') ? null : n.id,
        uid: n.uid,
        text: n.text,
        color: n.color,
        pinned: n.pinned,
        unlock_at: n.unlock_at ?? null,
        at: n.at,
      }));

    const serializeTodos = (list: BoardTodo[]) =>
      list.map((t) => ({
        id: String(t.id).startsWith('temp_') ? null : t.id,
        text: t.text,
        category: t.category ?? '日常',
        done: t.done,
        done_by: t.done_by ?? null,
        at: t.at,
      }));

    boardSyncChainRef.current = boardSyncChainRef.current.then(async () => {
      const snap = latestBundleRef.current;
      if (!snap) return;
      const target = computeTarget(snap.board.data);
      try {
        const updated = await api.updateBoard(
          space.id,
          snap.board.version,
          serializeNotes(target.notes),
          serializeTodos(target.todos),
        );
        latestBundleRef.current = updated;
        onBundleChange(updated);
      } catch (err) {
        const msg = err instanceof Error ? err.message : '';
        if (msg.includes('更新过') || msg.includes('409')) {
          try {
            const fresh = await api.getFridgeBundle(space.id);
            const merged = computeTarget(fresh.board.data);
            const retried = await api.updateBoard(
              space.id,
              fresh.board.version,
              serializeNotes(merged.notes),
              serializeTodos(merged.todos),
            );
            latestBundleRef.current = retried;
            onBundleChange(retried);
            return;
          } catch {
            // fall through to failure marker
          }
        }
        if (tempIdsToMarkFailed.length > 0 && latestBundleRef.current) {
          const curFail = latestBundleRef.current;
          const marked: FridgeBundleResp = {
            ...curFail,
            board: {
              ...curFail.board,
              data: {
                notes: curFail.board.data.notes.map((n) =>
                  tempIdsToMarkFailed.includes(n.id) ? { ...n, _transferState: 'failed' } : n,
                ),
                todos: curFail.board.data.todos.map((t) =>
                  tempIdsToMarkFailed.includes(t.id) ? { ...t, _transferState: 'failed' } : t,
                ),
              },
            },
          };
          latestBundleRef.current = marked;
          onBundleChange(marked);
        } else {
          toast('同步稍慢，请重试', 'error');
        }
      }
    });
  };

  const handleAddNote = (e: React.FormEvent) => {
    e.preventDefault();
    const cleanText = noteText.trim();
    if (!bundle || !cleanText) return;

    const unlockTs = noteUnlockTime
      ? new Date(
          noteUnlockTime.length === 16 ? `${noteUnlockTime}:00+08:00` : noteUnlockTime,
        ).getTime()
      : null;
    const validUnlock = unlockTs && !Number.isNaN(unlockTs) ? unlockTs : null;
    const tempId = `temp_note_${Date.now()}_${Math.random().toString(36).slice(2, 6)}`;

    const optimisticNote: BoardNote = {
      id: tempId,
      uid: selfUserId,
      text: cleanText,
      color: noteColor,
      pinned: false,
      unlock_at: validUnlock,
      locked: Boolean(validUnlock && validUnlock > Date.now()),
      at: Date.now(),
      _transferState: 'sending',
    };

    // 0ms WeChat-style clear & stick onto board immediately!
    setNoteText('');
    setNoteUnlockTime('');
    enqueueBoardMutation(
      (base) => ({
        notes: [optimisticNote, ...base.notes.filter((x) => x.id !== tempId)],
        todos: base.todos,
      }),
      [tempId],
    );
  };

  const handleMutateBoard = (nextNotes: BoardNote[], nextTodos: BoardTodo[]) => {
    if (!bundle) return;
    enqueueBoardMutation(() => ({ notes: nextNotes, todos: nextTodos }));
  };

  const handleAddTodo = (e: React.FormEvent) => {
    e.preventDefault();
    const cleanTodo = todoText.trim();
    if (!bundle || !cleanTodo) return;

    const tempId = `temp_todo_${Date.now()}_${Math.random().toString(36).slice(2, 6)}`;
    const optimisticTodo: BoardTodo = {
      id: tempId,
      text: cleanTodo,
      category: todoCat,
      done: false,
      at: Date.now(),
      _transferState: 'sending',
    };

    setTodoText('');
    enqueueBoardMutation(
      (base) => ({
        notes: base.notes,
        todos: [optimisticTodo, ...base.todos.filter((x) => x.id !== tempId)],
      }),
      [tempId],
    );
  };

  const filteredDishes: MenuDish[] =
    activeCat === '全部'
      ? menu.dishes
      : menu.dishes.filter((d) => d.category === activeCat);

  const isSplit = stage === 'split_board' || stage === 'split_menu';

  return (
    <>
      {/* =====================================================================
          STAGE 1: Bottom-Right Miniature Double-Door Refrigerator
         ===================================================================== */}
      <div
        style={{
          bottom: 'max(1.125rem, env(safe-area-inset-bottom))',
          right: 'calc(max(1rem, env(safe-area-inset-right)) + var(--kd-sbw, 0px))',
        }}
        className={`fixed z-30 flex flex-col items-center transition-opacity duration-200 ${
          stage !== 'collapsed' ? 'opacity-0 pointer-events-none' : 'opacity-100'
        }`}
      >
        {isNightBeijing && (
          <div
            aria-hidden
            className="absolute left-1/2 -translate-x-1/2 -top-16 w-44 h-44 rounded-full blur-md pointer-events-none animate-nightlight"
            style={{
              background:
                'radial-gradient(circle at 50% 50%, rgba(248, 198, 120, 0.24) 0%, rgba(248, 198, 120, 0.11) 28%, rgba(248, 198, 120, 0.03) 50%, rgba(248, 198, 120, 0) 68%)',
            }}
          />
        )}

        <button
          ref={fridgeAnchorRef}
          type="button"
          onClick={onOpenFridge}
          aria-label="打开冰箱"
          style={
            isNightBeijing
              ? {
                  boxShadow:
                    '0 22px 48px rgba(0, 0, 0, 0.52), 0 -4px 18px rgba(248, 198, 120, 0.08), inset 0 1px 0 rgba(252, 218, 162, 0.22)',
                }
              : undefined
          }
          className="kd-fridge-chassis relative w-[58px] h-[92px] sm:w-[66px] sm:h-[104px] p-[2px] rounded-t-[15px] rounded-b-[7px] flex flex-col overflow-visible transition-transform duration-200 ease-out hover:-translate-y-0.5 active:scale-[0.98] transform-gpu cursor-pointer"
        >
          {partnerInFridge && (
            <span
              title="TA 正在打开冰箱"
              className="absolute -right-[2px] top-3 bottom-2 w-[2.5px] rounded-full bg-amber-300 shadow-[0_0_12px_3px_rgba(251,191,36,0.65)] animate-pulse"
            />
          )}

          {/* Mini Raised Upper Freezer Door */}
          <div className="kd-fridge-door relative h-[35%] w-full rounded-t-[13px] rounded-b-[2px] flex items-center justify-end gap-1 pr-1.5 pl-3">
            <span className="kd-fridge-handle absolute left-1.5 bottom-1.5 w-[2.5px] h-4 rounded-full" />
            <span className="relative w-3.5 h-4 rounded-[1.5px] bg-paper-oat border border-border shadow-xs -rotate-3">
              <span className="kd-magnet-charcoal absolute -top-[1.5px] left-1/2 -translate-x-1/2 w-1.5 h-1.5 rounded-full" />
            </span>
            <span className="relative w-3.5 h-4 rounded-[1.5px] bg-paper-white border border-border shadow-xs rotate-2">
              <span className="kd-magnet-terracotta absolute -top-[1.5px] left-1/2 -translate-x-1/2 w-1.5 h-1.5 rounded-full" />
            </span>
          </div>

          {/* Physical Recessed Middle Gasket Channel */}
          <div className="kd-fridge-seam relative h-[3px] w-full shrink-0">
            <span className="absolute right-1 top-[0.5px] w-1.5 h-[2px] rounded-full bg-black/25" />
          </div>

          {/* Mini Raised Lower Chiller Door */}
          <div className="kd-fridge-door relative flex-1 w-full rounded-t-[2px] rounded-b-[5px] pt-1.5 pr-1.5 pl-3.5 flex items-start justify-end">
            <span className="kd-fridge-handle absolute left-1.5 top-2 w-[2.5px] h-6 rounded-full" />
            <div className="grid grid-cols-2 gap-[2.5px]">
              {slips.slice(0, 5).map((s) => (
                <span
                  key={s.id}
                  className={`w-2.5 h-2.5 rounded-[1px] border border-border shadow-2xs ${
                    s.done ? 'bg-paper-sage' : 'bg-paper-white'
                  }`}
                />
              ))}
            </div>
          </div>
        </button>

        {/* Appliance Leveling Feet & Floor Shadow */}
        <div className="w-[76%] flex justify-between px-1.5">
          <span className="w-2.5 h-[3px] rounded-b-[2px] bg-black/50" />
          <span className="w-2.5 h-[3px] rounded-b-[2px] bg-black/50" />
        </div>
        <span aria-hidden className="w-[88%] h-1.5 rounded-full bg-black/22 blur-[3px]" />
      </div>

      {/* =====================================================================
          STAGE 2 (Standing Double-Door Refrigerator) &
          STAGE 3 (Left Standing Refrigerator + Right Floating Paper Sheet!)
         ===================================================================== */}
      <AnimatePresence>
        {stage !== 'collapsed' && (
          <motion.div
            initial={{ opacity: 0 }}
            animate={{ opacity: 1 }}
            exit={{ opacity: 0 }}
            transition={{ duration: 0.16, ease: 'easeOut' }}
            onClick={() => onChangeStage('collapsed')}
            className={`fixed inset-0 z-40 bg-black/50 backdrop-blur-sm flex items-center md:justify-center md:py-7 md:px-10 lg:px-14 xl:px-20 ${
              !isSplit
                ? 'max-md:justify-start max-md:px-5 max-md:py-6'
                : 'max-md:justify-center max-md:p-0'
            }`}
          >
            <motion.div
              initial={{ opacity: 0, scale: 0.985 }}
              animate={{ opacity: 1, scale: 1 }}
              exit={{ opacity: 0, scale: 0.985 }}
              transition={{ duration: 0.18, ease: [0.22, 1, 0.36, 1] }}
              className={`flex flex-col md:flex-row md:items-stretch md:w-full md:max-w-[1680px] md:h-[86dvh] md:min-h-[620px] md:max-h-[820px] gap-0 md:gap-6 lg:gap-8 pointer-events-none transform-gpu ${
                !isSplit
                  ? 'max-md:h-[82dvh] max-md:min-h-[540px] max-md:max-h-[820px] max-md:items-start'
                  : 'max-md:w-full max-md:h-[100dvh]'
              }`}
            >
              {/* =============================================================
                  THE PHYSICAL TALL STANDING DOUBLE-DOOR REFRIGERATOR (LEFT-ANCHORED)
                  100% invariant desktop classes across Stage 2 & Stage 3 -> 0px twitch!
                 ============================================================= */}
              <div
                onClick={(e) => e.stopPropagation()}
                className={`flex flex-col items-center md:w-[336px] lg:w-[360px] md:h-full shrink-0 pointer-events-auto ${
                  !isSplit ? 'max-md:h-full' : 'max-md:w-full'
                }`}
              >
                <div
                  className={`kd-fridge-chassis relative flex flex-col overflow-hidden md:w-full md:flex-1 md:min-h-0 md:p-[4px] md:rounded-t-[28px] md:rounded-b-[12px] ${
                    !isSplit
                      ? 'max-md:w-[84vw] max-md:max-w-[344px] max-md:flex-1 max-md:min-h-0 max-md:p-[4px] max-md:rounded-t-[28px] max-md:rounded-b-[12px]'
                      : 'max-md:w-full max-md:p-0 max-md:border-b max-md:border-fridge-edge'
                  }`}
                >
                  {/* ── UPPER FREEZER COMPARTMENT DOOR (3D Raised Cushion) ── */}
                  <div
                    className={`kd-fridge-door relative shrink-0 md:rounded-t-[24px] md:rounded-b-[4px] md:pt-4.5 md:pb-6 md:pl-9 md:pr-5 ${
                      !isSplit
                        ? 'max-md:rounded-t-[24px] max-md:rounded-b-[4px] max-md:pt-4 max-md:pb-6 max-md:pl-9 max-md:pr-5'
                        : 'max-md:pt-safe max-md:pb-3.5 max-md:pl-8 max-md:pr-4'
                    }`}
                  >
                    {/* Machined Cylindrical Freezer Handle + Standoff Mounts */}
                    <div
                      aria-hidden
                      className={`absolute md:left-3 md:bottom-4 md:w-[7px] md:h-20 flex flex-col items-center justify-between md:py-1.5 ${
                        !isSplit
                          ? 'max-md:left-3 max-md:bottom-4 max-md:w-[7px] max-md:h-20 max-md:py-1.5'
                          : 'max-md:left-2.5 max-md:bottom-3 max-md:w-[6px] max-md:h-12 max-md:py-1'
                      }`}
                    >
                      <span className="w-full h-[2.5px] rounded-full bg-black/22" />
                      <span className="w-full h-[2.5px] rounded-full bg-black/22" />
                      <span className="kd-fridge-handle absolute inset-y-0 left-1/2 -translate-x-1/2 w-[5.5px] rounded-full" />
                    </div>

                    {/* Engraved Appliance Emblem + BLUE X (Always Closes Entire Fridge!) */}
                    <div className="flex items-center justify-between mb-3">
                      <div className="flex items-center gap-2">
                        <span className="inline-block px-2 py-0.5 rounded-[3px] bg-overlay/75 border border-border text-[8.5px] font-semibold tracking-[0.3em] text-muted select-none">
                          KEEPDAYS
                        </span>
                        {partnerInFridge && (
                          <span
                            title="TA 也在看冰箱"
                            className="w-1.5 h-1.5 rounded-full bg-amber-400 shadow-[0_0_8px_rgba(251,191,36,0.85)]"
                          />
                        )}
                      </div>
                      <button
                        type="button"
                        onClick={() => onChangeStage('collapsed')}
                        title="关上冰箱"
                        aria-label="关闭冰箱"
                        className="w-6.5 h-6.5 rounded-chip bg-overlay/70 hover:bg-border-strong/30 border border-border flex items-center justify-center text-muted hover:text-ink transition-colors cursor-pointer"
                      >
                        <X className="w-3.5 h-3.5" />
                      </button>
                    </div>

                    {/* Two Main Magnetic Paper Notes on Upper Freezer Door */}
                    <div className="grid grid-cols-2 gap-2.5 sm:gap-3">
                      <button
                        type="button"
                        onClick={() => onChangeStage('split_board')}
                        className={`group relative text-left p-3 sm:p-3.5 rounded-[3px] bg-paper-oat border -rotate-[1deg] transition-[border-color,box-shadow,transform] duration-150 ease-out hover:-translate-y-0.5 shadow-paper transform-gpu cursor-pointer ${
                          stage === 'split_board'
                            ? 'border-accent ring-1 ring-accent/35'
                            : 'border-border'
                        }`}
                      >
                        <span className="kd-magnet-charcoal w-3 h-3 rounded-full block mx-auto mb-2" />
                        <div className="font-editorial text-xs sm:text-sm font-medium text-ink">
                          留言 · 待办
                        </div>
                        <div className="text-[11px] text-muted mt-1 line-clamp-1">
                          {previewNote
                            ? `“${previewNote}”`
                            : `${board.notes.length} 便签 · ${openTodoCount} 待办`}
                        </div>
                      </button>

                      <button
                        type="button"
                        onClick={() => onChangeStage('split_menu')}
                        className={`group relative text-left p-3 sm:p-3.5 rounded-[3px] bg-paper-white border rotate-[0.8deg] transition-[border-color,box-shadow,transform] duration-150 ease-out hover:-translate-y-0.5 shadow-paper transform-gpu cursor-pointer ${
                          stage === 'split_menu'
                            ? 'border-accent ring-1 ring-accent/35'
                            : 'border-border'
                        }`}
                      >
                        <span className="kd-magnet-terracotta w-3 h-3 rounded-full block mx-auto mb-2" />
                        <div className="font-editorial text-xs sm:text-sm font-medium text-ink">
                          家庭菜单
                        </div>
                        <div className="text-[11px] text-muted mt-1 truncate">
                          {menu.dishes.length > 0
                            ? `${menu.dishes.length} 道家常菜`
                            : '点此写菜谱点菜'}
                        </div>
                      </button>
                    </div>
                  </div>

                  {/* ── RECESSED CENTER GASKET CHANNEL + RIGHT HINGE PIN ── */}
                  <div className="kd-fridge-seam relative h-[7px] w-full shrink-0 flex items-center justify-end pr-3">
                    <span className="w-4 h-[3px] rounded-full bg-border-strong" />
                  </div>

                  {/* ── LOWER MAIN CHILLER COMPARTMENT DOOR (3D Raised Cushion) ── */}
                  <div
                    className={`kd-fridge-door relative md:flex-1 md:min-h-0 md:rounded-t-[4px] md:rounded-b-[9px] md:overflow-y-auto md:pt-6 md:pb-7 md:pl-9 md:pr-5 ${
                      !isSplit
                        ? 'max-md:flex-1 max-md:min-h-0 max-md:rounded-t-[4px] max-md:rounded-b-[9px] max-md:overflow-y-auto max-md:pt-6 max-md:pb-7 max-md:pl-9 max-md:pr-5'
                        : 'max-md:py-3 max-md:pl-8 max-md:pr-4'
                    }`}
                  >
                    {/* Machined Cylindrical Chiller Handle + Standoff Mounts */}
                    <div
                      aria-hidden
                      className={`absolute md:left-3 md:top-6 md:w-[7px] md:h-36 flex flex-col items-center justify-between md:py-2 ${
                        !isSplit
                          ? 'max-md:left-3 max-md:top-6 max-md:w-[7px] max-md:h-36 max-md:py-2'
                          : 'max-md:left-2.5 max-md:top-3.5 max-md:w-[6px] max-md:h-14 max-md:py-1'
                      }`}
                    >
                      <span className="w-full h-[2.5px] rounded-full bg-black/22" />
                      <span className="w-full h-[2.5px] rounded-full bg-black/22" />
                      <span className="kd-fridge-handle absolute inset-y-0 left-1/2 -translate-x-1/2 w-[5.5px] rounded-full" />
                    </div>

                    {slips.length === 0 ? (
                      <div
                        onClick={() => onChangeStage('split_menu')}
                        className={`md:h-full md:flex md:items-center md:justify-center md:pb-8 ${
                          !isSplit
                            ? 'max-md:h-full max-md:flex max-md:items-center max-md:justify-center max-md:pb-8'
                            : 'max-md:py-3'
                        }`}
                      >
                        <div className="group px-6 py-5 rounded-[4px] border border-dashed border-border-strong/70 hover:border-border-strong transition-colors text-center cursor-pointer select-none">
                          <span className="kd-magnet-muted w-2.5 h-2.5 rounded-full block mx-auto mb-2.5 opacity-75" />
                          <div className="text-xs text-muted group-hover:text-ink transition-colors">
                            门上还没有贴今日菜单
                          </div>
                          <div className="text-[10px] text-subtle mt-1">
                            轻触前往家庭菜单点菜
                          </div>
                        </div>
                      </div>
                    ) : (
                      <div
                        className={`md:grid md:grid-cols-2 md:gap-3 md:overflow-visible md:pb-0 ${
                          isSplit
                            ? 'max-md:flex max-md:gap-2.5 max-md:overflow-x-auto max-md:pb-0.5 [scrollbar-width:none] [&::-webkit-scrollbar]:hidden'
                            : 'max-md:grid max-md:grid-cols-2 max-md:gap-3'
                        }`}
                      >
                        {slips.slice(0, 5).map((slip, idx) => {
                          const author = users[slip.by_uid]?.profile.nickname || '';
                          const tilt = idx % 3 === 0 ? -1.2 : idx % 3 === 1 ? 1.1 : -0.4;
                          return (
                            <div
                              key={slip.id}
                              style={{ transform: `rotate(${tilt}deg)` }}
                              className={`relative rounded-[2px] bg-paper-white border border-border shadow-paper flex flex-col justify-between md:min-w-0 md:p-2.5 md:gap-2 ${
                                isSplit
                                  ? 'max-md:min-w-[144px] max-md:shrink-0 max-md:p-2 max-md:gap-1.5'
                                  : 'max-md:p-2.5 max-md:gap-2'
                              }`}
                            >
                              {/* 3D Ceramic Pin Magnet */}
                              <span
                                className={`w-2.5 h-2.5 rounded-full mx-auto -mt-1 ${
                                  slip.done ? 'kd-magnet-muted' : 'kd-magnet-terracotta'
                                }`}
                              />

                              <div className="space-y-1">
                                <div className="flex items-baseline justify-between gap-1 text-[10px] text-muted">
                                  <span className="font-medium text-ink truncate">
                                    {slip.date_label.replace(' 今日菜单', '')}
                                  </span>
                                  {author && <span className="truncate">{author}</span>}
                                </div>

                                <div
                                  className={`text-xs space-y-0.5 ${
                                    slip.done ? 'line-through text-subtle' : 'text-ink'
                                  }`}
                                >
                                  {slip.dishes.map((dName, i) => (
                                    <div key={i} className="truncate">
                                      {dName}
                                    </div>
                                  ))}
                                </div>

                                {slip.note && (
                                  <div className="text-[10px] text-muted line-clamp-1 pt-0.5">
                                    {slip.note}
                                  </div>
                                )}
                              </div>

                              <div className="pt-1.5 border-t border-border flex items-center justify-between text-[10px]">
                                <button
                                  type="button"
                                  title="将食材加入待办清单"
                                  onClick={async () => {
                                    try {
                                      const res = await api.convertSlipToGrocery(
                                        space.id,
                                        slip.id,
                                      );
                                      onBundleChange(res);
                                      onChangeStage('split_board');
                                      toast('食材已加入待办', 'success');
                                    } catch (err) {
                                      await refreshOnConflict(err);
                                    }
                                  }}
                                  className={`p-0.5 transition-colors cursor-pointer ${
                                    slip.grocery_synced
                                      ? 'text-accent'
                                      : 'text-subtle hover:text-ink'
                                  }`}
                                >
                                  <ShoppingBag className="w-3 h-3" />
                                </button>

                                <button
                                  type="button"
                                  onClick={async () => {
                                    try {
                                      const res = await api.toggleSlipDone(
                                        space.id,
                                        slip.id,
                                        !slip.done,
                                      );
                                      onBundleChange(res);
                                    } catch (err) {
                                      await refreshOnConflict(err);
                                    }
                                  }}
                                  className={`px-1.5 py-0.5 rounded-[3px] transition-colors cursor-pointer ${
                                    slip.done
                                      ? 'text-accent bg-accent-soft font-medium'
                                      : 'text-muted hover:text-ink'
                                  }`}
                                >
                                  {slip.done ? '已完食' : '完食'}
                                </button>
                              </div>
                            </div>
                          );
                        })}
                      </div>
                    )}
                  </div>
                </div>

                {/* Appliance Leveling Feet */}
                <div
                  className={`md:flex w-[84%] justify-between px-3 shrink-0 ${
                    !isSplit ? 'max-md:flex' : 'max-md:hidden'
                  }`}
                >
                  <span className="w-5 h-1.5 rounded-b-[3px] bg-black/45" />
                  <span className="w-5 h-1.5 rounded-b-[3px] bg-black/45" />
                </div>
              </div>

              {/* =============================================================
                  UNFOLDED PAPER WORKSPACE (Floating Crisp Sheet Beside the Fridge!)
                 ============================================================= */}
              <AnimatePresence>
                {isSplit && (
                  <motion.div
                    key="fridge-paper-sheet"
                    initial={{ opacity: 0, x: 10 }}
                    animate={{ opacity: 1, x: 0 }}
                    exit={{ opacity: 0, x: 8 }}
                    transition={{ duration: 0.16, ease: [0.22, 1, 0.36, 1] }}
                    onClick={(e) => e.stopPropagation()}
                    className={`flex-1 min-h-0 h-full md:rounded-[14px] bg-panel border-t md:border border-border shadow-float overflow-y-auto overscroll-contain px-5 pt-5 sm:px-8 sm:pt-7 lg:px-11 pointer-events-auto transform-gpu ${
                      !isSplit ? 'max-md:hidden' : ''
                    }`}
                  >
                  {stage === 'split_board' ? (
                    <div className="w-full min-h-full flex flex-col">
                      <div className="space-y-7">
                      <div className="flex items-center justify-between border-b border-border pb-3.5">
                        <h3 className="font-editorial text-xl text-ink">留言与待办</h3>
                        <button
                          type="button"
                          onClick={() => onChangeStage('door')}
                          title="收起子页"
                          aria-label="关闭子页"
                          className="w-7 h-7 rounded-chip hover:bg-overlay flex items-center justify-center text-muted hover:text-ink transition-colors cursor-pointer"
                        >
                          <X className="w-4 h-4" />
                        </button>
                      </div>

                      {/* Compose Note */}
                      <form
                        onSubmit={handleAddNote}
                        className="p-4 rounded-input bg-surface border border-border space-y-3"
                      >
                        <textarea
                          rows={2}
                          value={noteText}
                          onChange={(e) => setNoteText(e.target.value)}
                          placeholder="留一张字条…"
                          className="w-full bg-transparent text-sm text-ink placeholder:text-subtle focus:outline-none resize-none"
                        />
                        <div className="flex flex-wrap items-center justify-between gap-2 pt-2 border-t border-border">
                          <div className="flex flex-wrap items-center gap-2">
                            {(['oat', 'cotton_white', 'sage', 'blush'] as const).map((c) => (
                              <button
                                key={c}
                                type="button"
                                onClick={() => setNoteColor(c)}
                                className={`w-4 h-4 rounded-full border ${PAPER_BG_CLASS[c]} ${
                                  noteColor === c ? 'border-ink scale-110' : 'border-border-strong'
                                } cursor-pointer`}
                              />
                            ))}
                            <label
                              title="可选：设定未来显影时间"
                              className="flex items-center gap-1 text-[11px] text-muted sm:ml-2 cursor-pointer"
                            >
                              <Clock className="w-3.5 h-3.5 shrink-0" />
                              <input
                                type="datetime-local"
                                value={noteUnlockTime}
                                onChange={(e) => setNoteUnlockTime(e.target.value)}
                                className="bg-transparent text-[11px] text-muted focus:outline-none max-w-[152px] sm:max-w-none"
                              />
                            </label>
                          </div>
                          <button
                            type="submit"
                            disabled={!noteText.trim()}
                            className="ml-auto px-4 py-1.5 rounded-chip bg-ink text-surface text-xs font-medium disabled:opacity-35 cursor-pointer"
                          >
                            贴上
                          </button>
                        </div>
                      </form>

                      {/* Notes */}
                      {board.notes.length > 0 && (
                        <div className="grid grid-cols-1 sm:grid-cols-2 gap-3.5">
                          {board.notes.map((n) => {
                            const author = users[n.uid]?.profile.nickname || '';
                            return (
                              <div
                                key={n.id}
                                className={`p-4 rounded-[4px] border border-border shadow-paper ${
                                  PAPER_BG_CLASS[n.color] || 'bg-paper-oat'
                                } flex flex-col justify-between min-h-[116px]`}
                              >
                                {n.locked && n.uid !== selfUserId ? (
                                  <div className="flex-1 flex flex-col items-center justify-center text-center py-3">
                                    <Lock className="w-4 h-4 text-accent mb-1.5" />
                                    <div className="text-xs font-medium text-ink">未到显影时刻</div>
                                    {n.unlock_at && (
                                      <div className="text-[11px] text-muted mt-0.5 tabular-nums">
                                        {new Date(n.unlock_at).toLocaleString('zh-CN', {
                                          month: '2-digit',
                                          day: '2-digit',
                                          hour: '2-digit',
                                          minute: '2-digit',
                                        })}
                                      </div>
                                    )}
                                  </div>
                                ) : (
                                  <p className="text-sm text-ink leading-relaxed whitespace-pre-wrap">
                                    {n.text}
                                    {n.locked && n.uid === selfUserId && (
                                      <span className="block text-[10px] text-accent mt-1.5">
                                        🔒 定时隐藏中
                                      </span>
                                    )}
                                  </p>
                                )}

                                <div className="flex items-center justify-between pt-3 mt-2 border-t border-border text-[11px] text-muted">
                                  <span className="inline-flex items-center gap-1.5">
                                    <span>{author}</span>
                                    {n._transferState === 'sending' && (
                                      <span className="inline-flex items-center gap-1 text-[10px] text-muted">
                                        <Loader2 className="w-2.5 h-2.5 animate-spin" />
                                        <span>传输中</span>
                                      </span>
                                    )}
                                    {n._transferState === 'failed' && (
                                      <button
                                        type="button"
                                        onClick={() =>
                                          enqueueBoardMutation(
                                            (base) => ({
                                              notes: base.notes.map((x) =>
                                                x.id === n.id
                                                  ? { ...x, _transferState: 'sending' }
                                                  : x,
                                              ),
                                              todos: base.todos,
                                            }),
                                            [n.id],
                                          )
                                        }
                                        className="inline-flex items-center gap-0.5 text-[10px] text-accent font-medium cursor-pointer"
                                      >
                                        <AlertCircle className="w-2.5 h-2.5" />
                                        <span>重发</span>
                                        <RotateCw className="w-2.5 h-2.5" />
                                      </button>
                                    )}
                                  </span>
                                  <div className="flex items-center gap-2">
                                    <button
                                      type="button"
                                      onClick={() => {
                                        const next = board.notes
                                          .map((x) =>
                                            x.id === n.id ? { ...x, pinned: !x.pinned } : x,
                                          )
                                          .sort((a, b) => Number(b.pinned) - Number(a.pinned));
                                        handleMutateBoard(next, board.todos);
                                      }}
                                      className={`cursor-pointer ${
                                        n.pinned ? 'text-accent' : 'text-subtle hover:text-ink'
                                      }`}
                                    >
                                      <Pin className="w-3.5 h-3.5" />
                                    </button>
                                    {!(n.locked && n.uid !== selfUserId) && (
                                      <button
                                        type="button"
                                        onClick={() =>
                                          handleMutateBoard(
                                            board.notes.filter((x) => x.id !== n.id),
                                            board.todos,
                                          )
                                        }
                                        className="text-subtle hover:text-accent cursor-pointer"
                                      >
                                        <Trash2 className="w-3.5 h-3.5" />
                                      </button>
                                    )}
                                  </div>
                                </div>
                              </div>
                            );
                          })}
                        </div>
                      )}

                      {/* Todos (Active List + Auto-Archive on Completion) */}
                      {(() => {
                        const openTodos = board.todos.filter((t) => !t.done);

                        return (
                          <div className="pt-4 border-t border-border space-y-3.5">
                            <div className="flex items-center justify-between">
                              <h4 className="font-editorial text-base text-ink">
                                待办清单
                                {openTodos.length > 0 && (
                                  <span className="ml-2 text-xs font-sans font-normal text-muted tabular-nums">
                                    {openTodos.length}
                                  </span>
                                )}
                              </h4>
                            </div>

                            <form onSubmit={handleAddTodo} className="flex items-center gap-2">
                              <CompactSelect
                                value={todoCat}
                                onChange={setTodoCat}
                                options={[
                                  { value: '日常', label: '日常', dotColor: '#8E867B' },
                                  { value: '买菜采购', label: '采购', dotColor: '#B85D43' },
                                  { value: '共同心愿', label: '心愿', dotColor: '#7C9473' },
                                ]}
                              />
                              <input
                                type="text"
                                value={todoText}
                                onChange={(e) => setTodoText(e.target.value)}
                                placeholder="添加事项…"
                                className="flex-1 min-w-0 h-9 px-3 text-sm bg-surface border border-border rounded-input text-ink placeholder:text-subtle focus:outline-none focus:border-border-strong"
                              />
                              <button
                                type="submit"
                                disabled={!todoText.trim()}
                                className="h-9 px-4 rounded-input bg-overlay hover:bg-border text-xs font-medium text-ink disabled:opacity-40 cursor-pointer shrink-0"
                              >
                                添加
                              </button>
                            </form>

                            {openTodos.length === 0 ? (
                              <div className="py-4 text-center text-xs text-subtle select-none">
                                当前没有未完成的待办事项
                              </div>
                            ) : (
                              <div className="divide-y divide-border">
                                {openTodos.map((t) => (
                                  <div
                                    key={t.id}
                                    className="group flex items-center justify-between py-2.5 text-sm"
                                  >
                                    <button
                                      type="button"
                                      onClick={() => {
                                        handleMutateBoard(
                                          board.notes,
                                          board.todos.map((x) =>
                                            x.id === t.id ? { ...x, done: true } : x,
                                          ),
                                        );
                                        toast('已完成并移入归档', 'info');
                                      }}
                                      className="flex items-center gap-2.5 text-left flex-1 min-w-0 cursor-pointer"
                                    >
                                      <Square className="w-4 h-4 text-subtle group-hover:text-accent transition-colors shrink-0" />
                                      <span className="text-ink truncate inline-flex items-center gap-1.5">
                                        {t.category && t.category !== '日常' && (
                                          <span className="text-[10px] px-1.5 py-0.5 rounded-chip bg-overlay text-muted mr-0.5">
                                            {t.category}
                                          </span>
                                        )}
                                        <span className="truncate">{t.text}</span>
                                        {t._transferState === 'sending' && (
                                          <Loader2 className="w-3 h-3 animate-spin text-muted shrink-0" />
                                        )}
                                      </span>
                                    </button>

                                    <button
                                      type="button"
                                      title="删除待办"
                                      onClick={() =>
                                        handleMutateBoard(
                                          board.notes,
                                          board.todos.filter((x) => x.id !== t.id),
                                        )
                                      }
                                      className="text-subtle hover:text-accent opacity-0 group-hover:opacity-100 transition-opacity cursor-pointer ml-2 shrink-0"
                                    >
                                      <Trash2 className="w-3.5 h-3.5" />
                                    </button>
                                  </div>
                                ))}
                              </div>
                            )}
                          </div>
                        );
                      })()}
                      </div>

                      {/* Bottom-Right Anchored Archive Trigger & Upward Popover */}
                      {(() => {
                        const archivedTodos = board.todos.filter((t) => t.done);
                        if (archivedTodos.length === 0) {
                          return <div className="pb-5" />;
                        }
                        return (
                          <div className="mt-auto sticky bottom-0 pt-5 pb-5 flex justify-end pointer-events-none z-10">
                            <div className="relative pointer-events-auto">
                              <AnimatePresence>
                                {showArchivedTodos && (
                                  <motion.div
                                    initial={{ opacity: 0, y: 6, scale: 0.98 }}
                                    animate={{ opacity: 1, y: 0, scale: 1 }}
                                    exit={{ opacity: 0, y: 6, scale: 0.98 }}
                                    transition={{ duration: 0.15, ease: [0.22, 1, 0.36, 1] }}
                                    className="absolute bottom-full right-0 mb-2 w-[290px] sm:w-[360px] max-h-64 overflow-y-auto rounded-card bg-panel border border-border shadow-float p-3.5 transform-gpu"
                                  >
                                    <div className="flex items-center justify-between pb-2 mb-1.5 border-b border-border">
                                      <span className="text-xs font-medium text-ink tabular-nums">
                                        已归档待办 ({archivedTodos.length})
                                      </span>
                                      <button
                                        type="button"
                                        onClick={() => {
                                          handleMutateBoard(
                                            board.notes,
                                            board.todos.filter((x) => !x.done),
                                          );
                                          setShowArchivedTodos(false);
                                        }}
                                        className="text-[11px] text-subtle hover:text-accent transition-colors cursor-pointer"
                                      >
                                        清空归档
                                      </button>
                                    </div>

                                    <div className="divide-y divide-border">
                                      {archivedTodos.map((t) => {
                                        const finisher = t.done_by
                                          ? users[t.done_by]?.profile.nickname
                                          : null;
                                        const doneTs = t.done_at || t.at;
                                        const doneDate = new Date(doneTs).toLocaleDateString(
                                          'zh-CN',
                                          { month: '2-digit', day: '2-digit' },
                                        );
                                        return (
                                          <div
                                            key={t.id}
                                            className="group flex items-center justify-between py-2 text-xs gap-2"
                                          >
                                            <button
                                              type="button"
                                              title="点击恢复到待办清单"
                                              onClick={() =>
                                                handleMutateBoard(
                                                  board.notes,
                                                  board.todos.map((x) =>
                                                    x.id === t.id ? { ...x, done: false } : x,
                                                  ),
                                                )
                                              }
                                              className="flex items-center gap-2 text-left flex-1 min-w-0 cursor-pointer"
                                            >
                                              <CheckSquare className="w-3.5 h-3.5 text-muted shrink-0" />
                                              <span className="line-through text-subtle truncate">
                                                {t.category && t.category !== '日常' && (
                                                  <span className="text-[10px] px-1.5 py-0.5 rounded-chip bg-overlay text-subtle mr-1.5 no-underline inline-block">
                                                    {t.category}
                                                  </span>
                                                )}
                                                {t.text}
                                              </span>
                                            </button>

                                            <div className="flex items-center gap-2 shrink-0 text-[11px] text-subtle tabular-nums">
                                              {finisher && <span>{finisher}</span>}
                                              <span>{doneDate}</span>
                                              <button
                                                type="button"
                                                title="恢复到待办"
                                                onClick={() =>
                                                  handleMutateBoard(
                                                    board.notes,
                                                    board.todos.map((x) =>
                                                      x.id === t.id ? { ...x, done: false } : x,
                                                    ),
                                                  )
                                                }
                                                className="text-subtle hover:text-ink opacity-0 group-hover:opacity-100 transition-opacity cursor-pointer"
                                              >
                                                <RotateCcw className="w-3 h-3" />
                                              </button>
                                              <button
                                                type="button"
                                                title="彻底删除"
                                                onClick={() =>
                                                  handleMutateBoard(
                                                    board.notes,
                                                    board.todos.filter((x) => x.id !== t.id),
                                                  )
                                                }
                                                className="text-subtle hover:text-accent opacity-0 group-hover:opacity-100 transition-opacity cursor-pointer"
                                              >
                                                <Trash2 className="w-3 h-3" />
                                              </button>
                                            </div>
                                          </div>
                                        );
                                      })}
                                    </div>
                                  </motion.div>
                                )}
                              </AnimatePresence>

                              <button
                                type="button"
                                onClick={() => setShowArchivedTodos((v) => !v)}
                                className={`inline-flex items-center gap-1.5 px-3 py-1.5 rounded-chip border text-xs transition-colors cursor-pointer select-none shadow-2xs ${
                                  showArchivedTodos
                                    ? 'bg-ink text-surface border-ink'
                                    : 'bg-surface/95 hover:bg-overlay border-border text-muted hover:text-ink'
                                }`}
                              >
                                <Archive className="w-3.5 h-3.5" />
                                <span className="tabular-nums">已归档 ({archivedTodos.length})</span>
                              </button>
                            </div>
                          </div>
                        );
                      })()}
                    </div>
                  ) : (
                    /* ── UNFOLDED FAMILY MENU SHEET ── */
                    <div className="w-full flex flex-col min-h-full">
                      <div className="flex items-center justify-between border-b border-border pb-3.5 mb-4">
                        <h3 className="font-editorial text-xl text-ink">家庭菜单</h3>
                        <div className="flex items-center gap-2">
                          <button
                            type="button"
                            onClick={() => setAddingDish((v) => !v)}
                            className="flex items-center gap-1 px-3 py-1.5 rounded-chip border border-border text-xs text-ink hover:border-border-strong cursor-pointer"
                          >
                            <Plus className="w-3.5 h-3.5" />
                            <span>新菜谱</span>
                          </button>
                          <button
                            type="button"
                            onClick={() => onChangeStage('door')}
                            title="收起子页"
                            aria-label="关闭子页"
                            className="w-7 h-7 rounded-chip hover:bg-overlay flex items-center justify-center text-muted hover:text-ink transition-colors cursor-pointer"
                          >
                            <X className="w-4 h-4" />
                          </button>
                        </div>
                      </div>

                      <div className="flex items-center gap-1.5 overflow-x-auto pb-3">
                        {['全部', ...menu.categories].map((cat) => (
                          <button
                            key={cat}
                            type="button"
                            onClick={() => setActiveCat(cat)}
                            className={`px-3 py-1 rounded-chip text-xs shrink-0 transition-colors cursor-pointer ${
                              activeCat === cat
                                ? 'bg-ink text-surface font-medium'
                                : 'bg-overlay text-muted hover:text-ink'
                            }`}
                          >
                            {cat}
                          </button>
                        ))}
                      </div>

                      {addingDish && (
                        <form
                          onSubmit={handleCreateDish}
                          className="mb-4 p-4 rounded-input bg-surface border border-border space-y-2.5"
                        >
                          <div className="grid grid-cols-1 sm:grid-cols-3 gap-2">
                            <input
                              type="text"
                              required
                              value={dishName}
                              onChange={(e) => setDishName(e.target.value)}
                              placeholder="菜名"
                              className="sm:col-span-2 h-9 px-3 text-sm bg-panel border border-border rounded-input text-ink focus:outline-none focus:border-border-strong"
                            />
                            <CompactSelect
                              value={dishCat}
                              onChange={setDishCat}
                              surfaceClass="bg-panel"
                              fullWidth
                              options={menu.categories.map((c) => ({ value: c, label: c }))}
                            />
                          </div>
                          <input
                            type="text"
                            value={dishIngredients}
                            onChange={(e) => setDishIngredients(e.target.value)}
                            placeholder="主要食材（空格分隔，选填）"
                            className="w-full px-3 py-2 text-xs bg-panel border border-border rounded-input text-ink"
                          />
                          <div className="flex flex-wrap items-center justify-between gap-2">
                            <input
                              type="text"
                              value={dishNote}
                              onChange={(e) => setDishNote(e.target.value)}
                              placeholder="做法或口味备注（选填）"
                              className="flex-1 min-w-[160px] px-3 py-2 text-xs bg-panel border border-border rounded-input text-ink"
                            />
                            <label className="px-3 py-2 rounded-input bg-overlay text-xs text-muted cursor-pointer">
                              {dishFile ? '已选配图 ✓' : '配图'}
                              <input
                                type="file"
                                accept="image/*"
                                onChange={(e) => setDishFile(e.target.files?.[0] || null)}
                                className="hidden"
                              />
                            </label>
                            <button
                              type="submit"
                              disabled={busy}
                              className="px-4 py-2 rounded-input bg-ink text-surface text-xs font-medium cursor-pointer"
                            >
                              保存
                            </button>
                          </div>
                        </form>
                      )}

                      {pawCommentary && (
                        <div className="mb-4 px-3.5 py-2.5 rounded-input bg-accent-soft text-xs text-ink">
                          {pawCommentary}
                        </div>
                      )}

                      <div className="flex-1">
                        {filteredDishes.length === 0 ? (
                          <div className="py-14 text-center text-xs text-subtle">
                            暂无菜品，点击右上角「新菜谱」添加家里会做的菜
                          </div>
                        ) : (
                          <div className="grid grid-cols-1 sm:grid-cols-2 gap-2.5">
                            {filteredDishes.map((dish) => {
                              const checked = checkedDishIds.includes(dish.id);
                              const coverUrl = resolveMediaUrl(dish.cover_asset_id, assetUrls, true);
                              return (
                                <div
                                  key={dish.id}
                                  onClick={() => toggleCheckDish(dish.id)}
                                  className={`group flex items-center gap-3 p-3.5 rounded-input border transition-colors cursor-pointer ${
                                    checked
                                      ? 'bg-accent-soft border-accent/60'
                                      : 'bg-surface border-border hover:border-border-strong'
                                  }`}
                                >
                                  {coverUrl && (
                                    <img
                                      src={coverUrl}
                                      alt={dish.name}
                                      className="w-12 h-12 rounded-[4px] object-cover shrink-0"
                                    />
                                  )}
                                  <div className="flex-1 min-w-0">
                                    <div className="flex items-baseline gap-2">
                                      <span className="text-sm font-medium text-ink truncate">
                                        {dish.name}
                                      </span>
                                      <span className="text-[11px] text-subtle shrink-0">
                                        {dish.category}
                                      </span>
                                    </div>
                                    {(dish.note || dish.ingredients.length > 0) && (
                                      <p className="text-xs text-muted truncate mt-0.5">
                                        {dish.note || dish.ingredients.join(' · ')}
                                      </p>
                                    )}
                                  </div>

                                  <div className="flex items-center gap-1.5 shrink-0">
                                    {!space.is_shared && otherSharedSpaces.length > 0 && (
                                      <button
                                        type="button"
                                        title={`同步至 ${otherSharedSpaces[0].name}`}
                                        onClick={(e) => {
                                          e.stopPropagation();
                                          api
                                            .projectDishToSpace(
                                              space.id,
                                              otherSharedSpaces[0].id,
                                              dish.id,
                                            )
                                            .then(() =>
                                              toast(
                                                `已同步至「${otherSharedSpaces[0].name}」`,
                                                'success',
                                              ),
                                            )
                                            .catch(() => toast('同步失败', 'error'));
                                        }}
                                        className="p-1 text-subtle hover:text-ink"
                                      >
                                        <ArrowUpRight className="w-3.5 h-3.5" />
                                      </button>
                                    )}
                                    <button
                                      type="button"
                                      onClick={(e) => {
                                        e.stopPropagation();
                                        handleDeleteDish(dish.id);
                                      }}
                                      className="p-1 text-subtle hover:text-accent opacity-0 group-hover:opacity-100 transition-opacity"
                                    >
                                      <Trash2 className="w-3.5 h-3.5" />
                                    </button>
                                    <span
                                      className={`w-4.5 h-4.5 rounded-[4px] flex items-center justify-center border transition-colors ${
                                        checked
                                          ? 'bg-accent border-accent text-white'
                                          : 'border-border-strong bg-panel'
                                      }`}
                                    >
                                      {checked && <Check className="w-3 h-3" />}
                                    </span>
                                  </div>
                                </div>
                              );
                            })}
                          </div>
                        )}
                      </div>

                      {/* Sticky Bottom Order Bar */}
                      <div className="sticky bottom-0 z-10 -mx-5 sm:-mx-8 mt-6 px-5 sm:px-8 pt-3 pb-safe bg-panel border-t border-border flex flex-col sm:flex-row items-stretch sm:items-center gap-2">
                        <input
                          type="text"
                          value={orderNote}
                          onChange={(e) => setOrderNote(e.target.value)}
                          placeholder="备注（如：少辣、七点半开饭）"
                          className="flex-1 px-3 py-2 text-xs bg-surface border border-border rounded-input text-ink placeholder:text-subtle focus:outline-none focus:border-border-strong"
                        />
                        <div className="flex items-center gap-2 shrink-0">
                          {space.is_shared && canPetPickMenu && (
                            <button
                              type="button"
                              onClick={handlePawPick}
                              className="flex items-center gap-1 px-3 py-2 rounded-input bg-overlay hover:bg-border text-xs text-ink cursor-pointer"
                            >
                              <PawPrint className="w-3.5 h-3.5 text-accent" />
                              <span>帮选</span>
                            </button>
                          )}
                          <button
                            type="button"
                            disabled={checkedDishIds.length === 0 || busy}
                            onClick={handleConfirmTodayMenu}
                            className="flex-1 sm:flex-initial px-4 py-2 rounded-input bg-ink text-surface text-xs font-medium disabled:opacity-35 cursor-pointer"
                          >
                            贴上今日菜单{checkedDishIds.length > 0 ? ` (${checkedDishIds.length})` : ''}
                          </button>
                        </div>
                      </div>
                    </div>
                  )}
                  </motion.div>
                )}
              </AnimatePresence>
            </motion.div>
          </motion.div>
        )}
      </AnimatePresence>
    </>
  );
};

interface CompactSelectOption {
  value: string;
  label: string;
  dotColor?: string;
}

const CompactSelect: React.FC<{
  value: string;
  options: CompactSelectOption[];
  onChange: (next: string) => void;
  surfaceClass?: string;
  fullWidth?: boolean;
}> = ({ value, options, onChange, surfaceClass = 'bg-surface', fullWidth = false }) => {
  const [open, setOpen] = useState(false);
  const wrapRef = useRef<HTMLDivElement | null>(null);
  const current = options.find((o) => o.value === value) || options[0];

  useEffect(() => {
    if (!open) return;
    const onPointerDown = (e: PointerEvent) => {
      if (wrapRef.current && !wrapRef.current.contains(e.target as Node)) {
        setOpen(false);
      }
    };
    window.addEventListener('pointerdown', onPointerDown);
    return () => window.removeEventListener('pointerdown', onPointerDown);
  }, [open]);

  return (
    <div ref={wrapRef} className={`relative shrink-0 ${fullWidth ? 'w-full' : ''}`}>
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        className={`h-9 px-3 rounded-input ${surfaceClass} border transition-colors inline-flex items-center justify-between gap-2 text-xs text-ink cursor-pointer select-none ${
          fullWidth ? 'w-full' : 'min-w-[78px]'
        } ${open ? 'border-border-strong' : 'border-border hover:border-border-strong'}`}
      >
        <span className="inline-flex items-center gap-1.5 truncate">
          {current?.dotColor && (
            <span
              style={{ backgroundColor: current.dotColor }}
              className="w-1.5 h-1.5 rounded-full shrink-0"
            />
          )}
          <span className="font-medium">{current?.label || value}</span>
        </span>
        <ChevronDown
          className={`w-3.5 h-3.5 text-muted shrink-0 transition-transform duration-150 ${
            open ? 'rotate-180' : ''
          }`}
        />
      </button>

      <AnimatePresence>
        {open && (
          <motion.div
            initial={{ opacity: 0, y: 4, scale: 0.98 }}
            animate={{ opacity: 1, y: 0, scale: 1 }}
            exit={{ opacity: 0, y: 4, scale: 0.98 }}
            transition={{ duration: 0.13, ease: [0.22, 1, 0.36, 1] }}
            className="absolute left-0 top-full mt-1.5 min-w-[108px] w-full rounded-[11px] bg-panel border border-border shadow-float p-1 z-30 transform-gpu"
          >
            {options.map((opt) => {
              const active = opt.value === value;
              return (
                <button
                  key={opt.value}
                  type="button"
                  onClick={() => {
                    onChange(opt.value);
                    setOpen(false);
                  }}
                  className={`w-full px-2.5 py-1.5 rounded-[7px] text-xs flex items-center justify-between gap-2 transition-colors cursor-pointer ${
                    active ? 'bg-overlay text-ink font-medium' : 'text-muted hover:bg-overlay/60 hover:text-ink'
                  }`}
                >
                  <span className="inline-flex items-center gap-1.5 truncate">
                    {opt.dotColor && (
                      <span
                        style={{ backgroundColor: opt.dotColor }}
                        className="w-1.5 h-1.5 rounded-full shrink-0"
                      />
                    )}
                    <span>{opt.label}</span>
                  </span>
                  {active && <Check className="w-3 h-3 text-accent shrink-0" />}
                </button>
              );
            })}
          </motion.div>
        )}
      </AnimatePresence>
    </div>
  );
};
