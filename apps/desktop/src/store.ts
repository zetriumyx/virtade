import { create } from 'zustand';
import { persist } from 'zustand/middleware';
interface UIState {
  theme: 'dark' | 'light';
  workspace: string;
  session: string;
  tab: 'chat' | 'files' | 'git' | 'plugins' | 'extension';
  extension: string;
  right: boolean;
  terminal: boolean;
  drafts: Record<string, string>;
  set: (patch: Partial<Omit<UIState, 'set'>>) => void;
}
export const useUI = create<UIState>()(
  persist(
    (set) => ({
      theme: 'dark',
      workspace: '',
      session: '',
      tab: 'chat',
      extension: '',
      right: true,
      terminal: false,
      drafts: {},
      set: (patch) => set(patch),
    }),
    { name: 'virtade.workbench.v1', version: 1 },
  ),
);
