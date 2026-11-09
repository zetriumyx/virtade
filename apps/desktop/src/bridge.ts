import { invoke, isTauri } from '@tauri-apps/api/core';
import type { DesktopBridge, Request } from './types';
export const nativeBridge: DesktopBridge = {
  available: isTauri(),
  async request<T>(request: Request): Promise<T> {
    if (!isTauri()) throw new Error('请在 virtade 桌面应用中连接本地工作区。');
    try {
      return await invoke<T>(
        'desktop_request',
        { version: 1, request },
        {
          headers: {
            'x-virtade-host': String(
              (window as unknown as Record<string, unknown>).__VIRTADE_HOST_TOKEN__ ?? '',
            ),
          },
        },
      );
    } catch (error) {
      throw new Error(
        typeof error === 'object' && error && 'message' in error
          ? String(error.message)
          : String(error),
      );
    }
  },
};
