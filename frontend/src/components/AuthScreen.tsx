import React, { useState } from 'react';
import { motion } from 'framer-motion';
import { ArrowRight, Moon, Sun, SunMoon } from 'lucide-react';
import type { ThemePreference } from '../hooks/useBeijingAmbient';
import { api, setAccessToken } from '../lib/apiClient';
import type { AuthSessionResp } from '../types/api';
import GradientWaves from './GradientWaves';

interface AuthScreenProps {
  isNight: boolean;
  themePreference: ThemePreference;
  onCycleTheme: () => void;
  onAuthenticated: (session: AuthSessionResp) => void;
}

export const AuthScreen: React.FC<AuthScreenProps> = ({
  isNight,
  themePreference,
  onCycleTheme,
  onAuthenticated,
}) => {
  const [mode, setMode] = useState<'login' | 'register'>('login');
  const [username, setUsername] = useState('');
  const [password, setPassword] = useState('');
  const [nickname, setNickname] = useState('');
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    setError(null);
    setLoading(true);
    try {
      const session =
        mode === 'login'
          ? await api.login(username, password)
          : await api.register(username, password, nickname || undefined);
      setAccessToken(session.access_token);
      onAuthenticated(session);
    } catch (err) {
      setError(err instanceof Error ? err.message : '操作失败');
    } finally {
      setLoading(false);
    }
  };

  return (
    <div
      style={{ backgroundColor: isNight ? '#191412' : '#F6EFE6' }}
      className="relative min-h-[100dvh] w-full flex items-center justify-center px-4 py-10 overflow-hidden transition-colors duration-500"
    >
      {/* Full-Screen Interactive Warm GradientWaves Backdrop */}
      <div className="fixed inset-0 pointer-events-auto">
        <GradientWaves
          horizonColor={isNight ? '#5C301E' : '#EAC49B'}
          waveColor={isNight ? '#C86D4C' : '#C96F4F'}
          crestColor={isNight ? '#F8DFB8' : '#FFFDF9'}
          backgroundColor={isNight ? '#191412' : '#F6EFE6'}
          lightMode={!isNight}
          speed={0.28}
          amplitude={2.2}
          waveScale={0.55}
          waveRatio={0.88}
          swell={34}
          turbulence={18}
          tilt={64}
          height={5.5}
          fogDepth={15}
          relief={0.92}
          lightAngle={124}
          sheen={isNight ? 0.42 : 0.32}
          brightness={isNight ? 1.02 : 1.0}
          opacity={isNight ? 0.92 : 0.88}
          grain={0.14}
          mouseInteraction
          swellOnClick
        />
      </div>

      {/* Soft Warm Ambient Vignette for Card Readability */}
      <div
        aria-hidden
        className="fixed inset-0 pointer-events-none"
        style={{
          background: isNight
            ? 'radial-gradient(circle at 50% 42%, rgba(25, 20, 18, 0.12) 0%, rgba(20, 16, 14, 0.46) 100%)'
            : 'radial-gradient(circle at 50% 38%, rgba(255, 251, 245, 0.32) 0%, rgba(242, 229, 212, 0.14) 100%)',
        }}
      />

      {/* Top-Right Ambient Theme Switcher */}
      <button
        type="button"
        onClick={onCycleTheme}
        title="切换明暗外观"
        className={`fixed top-4 right-4 z-20 w-9 h-9 rounded-chip backdrop-blur-md border flex items-center justify-center transition-colors cursor-pointer ${
          isNight
            ? 'bg-[#26201C]/80 border-[#F6EFE6]/14 text-[#CFC5B9] hover:text-[#FAF5EE]'
            : 'bg-[#FFFBF6]/80 border-[#3E2A1E]/12 text-[#7A685B] hover:text-[#241B16]'
        }`}
      >
        {themePreference === 'auto' ? (
          <SunMoon className="w-4 h-4" />
        ) : themePreference === 'daylight' ? (
          <Sun className="w-4 h-4" />
        ) : (
          <Moon className="w-4 h-4" />
        )}
      </button>

      {/* Warm Frosted Parchment Auth Card */}
      <motion.div
        initial={{ opacity: 0, y: 10 }}
        animate={{ opacity: 1, y: 0 }}
        transition={{ duration: 0.3, ease: [0.22, 1, 0.36, 1] }}
        className={`relative z-10 w-full max-w-[388px] backdrop-blur-xl rounded-[20px] p-7 sm:p-9 border transform-gpu transition-colors duration-300 ${
          isNight
            ? 'bg-[#211C19]/84 border-[#F6EFE6]/12 text-[#F3EDE4] shadow-[0_32px_80px_rgba(0,0,0,0.52),inset_0_1px_0_rgba(255,248,238,0.08)]'
            : 'bg-[#FFFDF9]/86 border-[#4A3222]/10 text-[#261E19] shadow-[0_28px_72px_rgba(92,48,28,0.13),inset_0_1px_0_rgba(255,255,255,0.85)]'
        }`}
      >
        <div className="mb-7">
          <div className="flex items-center gap-2 mb-2">
            <span className="w-2 h-2 rounded-full bg-[#C66B4B]" />
            <span
              className={`text-[10px] font-medium tracking-[0.26em] uppercase ${
                isNight ? 'text-[#B5A798]' : 'text-[#8C7667]'
              }`}
            >
              HOME ARCHIVE
            </span>
          </div>
          <h1 className="font-editorial text-3xl tracking-tight">KeepDays</h1>
          <p className={`text-xs mt-2 ${isNight ? 'text-[#A99D90]' : 'text-[#7D6C5E]'}`}>
            {mode === 'login' ? '推门回家，继续记录日常。' : '建立独立空间与邀请门牌号。'}
          </p>
        </div>

        <div
          className={`grid grid-cols-2 p-1 rounded-input mb-6 text-xs font-medium ${
            isNight ? 'bg-[#161311]/75' : 'bg-[#F2EAE0]/85'
          }`}
        >
          <button
            type="button"
            onClick={() => {
              setMode('login');
              setError(null);
            }}
            className={`py-2 rounded-slip transition-colors duration-150 cursor-pointer ${
              mode === 'login'
                ? isNight
                  ? 'bg-[#2E2723] text-[#FAF5EE] shadow-xs'
                  : 'bg-[#FFFDF9] text-[#241B16] shadow-xs'
                : isNight
                  ? 'text-[#9E9285] hover:text-[#F3EDE4]'
                  : 'text-[#827062] hover:text-[#241B16]'
            }`}
          >
            登录
          </button>
          <button
            type="button"
            onClick={() => {
              setMode('register');
              setError(null);
            }}
            className={`py-2 rounded-slip transition-colors duration-150 cursor-pointer ${
              mode === 'register'
                ? isNight
                  ? 'bg-[#2E2723] text-[#FAF5EE] shadow-xs'
                  : 'bg-[#FFFDF9] text-[#241B16] shadow-xs'
                : isNight
                  ? 'text-[#9E9285] hover:text-[#F3EDE4]'
                  : 'text-[#827062] hover:text-[#241B16]'
            }`}
          >
            注册
          </button>
        </div>

        <form onSubmit={handleSubmit} className="space-y-4">
          <div>
            <label
              className={`block text-xs mb-1.5 ${isNight ? 'text-[#B2A598]' : 'text-[#786658]'}`}
            >
              账号
            </label>
            <input
              type="text"
              required
              minLength={3}
              autoComplete="username"
              value={username}
              onChange={(e) => setUsername(e.target.value)}
              placeholder="用户名或邮箱"
              className={`w-full px-3.5 py-2.5 text-sm rounded-input border focus:outline-none transition-colors ${
                isNight
                  ? 'bg-[#171412]/80 border-[#F6EFE6]/12 text-[#F5EFE6] placeholder:text-[#6E645B] focus:border-[#D4765B]/70'
                  : 'bg-[#FAF6F0]/90 border-[#4A3222]/12 text-[#241B16] placeholder:text-[#A89889] focus:border-[#B85D43]/60'
              }`}
            />
          </div>

          {mode === 'register' && (
            <div>
              <label
                className={`block text-xs mb-1.5 ${isNight ? 'text-[#B2A598]' : 'text-[#786658]'}`}
              >
                昵称
              </label>
              <input
                type="text"
                value={nickname}
                onChange={(e) => setNickname(e.target.value)}
                placeholder="日常称呼（选填）"
                className={`w-full px-3.5 py-2.5 text-sm rounded-input border focus:outline-none transition-colors ${
                  isNight
                    ? 'bg-[#171412]/80 border-[#F6EFE6]/12 text-[#F5EFE6] placeholder:text-[#6E645B] focus:border-[#D4765B]/70'
                    : 'bg-[#FAF6F0]/90 border-[#4A3222]/12 text-[#241B16] placeholder:text-[#A89889] focus:border-[#B85D43]/60'
                }`}
              />
            </div>
          )}

          <div>
            <label
              className={`block text-xs mb-1.5 ${isNight ? 'text-[#B2A598]' : 'text-[#786658]'}`}
            >
              密码
            </label>
            <input
              type="password"
              required
              minLength={8}
              autoComplete={mode === 'login' ? 'current-password' : 'new-password'}
              value={password}
              onChange={(e) => setPassword(e.target.value)}
              placeholder="至少 8 位字符"
              className={`w-full px-3.5 py-2.5 text-sm rounded-input border focus:outline-none transition-colors ${
                isNight
                  ? 'bg-[#171412]/80 border-[#F6EFE6]/12 text-[#F5EFE6] placeholder:text-[#6E645B] focus:border-[#D4765B]/70'
                  : 'bg-[#FAF6F0]/90 border-[#4A3222]/12 text-[#241B16] placeholder:text-[#A89889] focus:border-[#B85D43]/60'
              }`}
            />
          </div>

          {error && (
            <div className="text-xs text-[#C45D41] bg-[#C45D41]/12 border border-[#C45D41]/25 rounded-input px-3 py-2">
              {error}
            </div>
          )}

          <button
            type="submit"
            disabled={loading}
            className={`w-full flex items-center justify-center gap-2 py-2.5 px-4 rounded-input text-sm font-medium disabled:opacity-50 transition-[background-color,opacity,transform] duration-150 active:scale-[0.99] cursor-pointer mt-2 shadow-xs ${
              isNight
                ? 'bg-[#C96E4E] hover:bg-[#D37959] text-[#FFFDF9]'
                : 'bg-[#B85D43] hover:bg-[#A65139] text-[#FFFDF9]'
            }`}
          >
            <span>{loading ? '验证中…' : mode === 'login' ? '进入' : '创建账号'}</span>
            <ArrowRight className="w-4 h-4" />
          </button>
        </form>
      </motion.div>
    </div>
  );
};
