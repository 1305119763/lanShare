import { invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { getCurrentWebview } from '@tauri-apps/api/webview'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { open } from '@tauri-apps/plugin-dialog'
import { openUrl } from '@tauri-apps/plugin-opener'

export type ShareFolder = {
  id: string
  name: string
  path: string
  enabled: boolean
  /** "dir" | "file" */
  kind?: string
}

export type AddShareResult = ShareFolder & {
  /** 共享目录内被自动过滤的相对路径（如 node_modules） */
  filteredDirs?: string[]
}

export type ServerLog = {
  id: string
  time: string
  level: string
  message: string
}

export type Settings = {
  port: number
  folders: ShareFolder[]
  allowUpload: boolean
}

export type ServerStatus = {
  running: boolean
  port: number
  ips: string[]
  urls: string[]
  logs: ServerLog[]
}

export type AppInfo = {
  version: string
  platform: string
  hostname: string
}

export const api = {
  getSettings: () => invoke<Settings>('get_settings'),
  setSettings: (patch: { port?: number; allowUpload?: boolean }) =>
    invoke<Settings>('set_settings', { patch }),
  addShare: (path: string) => invoke<AddShareResult>('add_share', { path }),
  addShares: (paths: string[]) => invoke<AddShareResult[]>('add_shares', { paths }),
  updateFolder: (id: string, patch: { name?: string; enabled?: boolean }) =>
    invoke<ShareFolder[]>('update_folder', { id, patch }),
  removeFolder: (id: string) => invoke<ShareFolder[]>('remove_folder', { id }),
  startServer: () => invoke<ServerStatus>('start_server'),
  stopServer: () => invoke<ServerStatus>('stop_server'),
  getServerStatus: () => invoke<ServerStatus>('get_server_status'),
  getAppInfo: () => invoke<AppInfo>('get_app_info'),
  openPath: (path: string) => invoke<void>('open_path', { path }),
  openExternal: (url: string) => openUrl(url),
  pickFolder: async (): Promise<AddShareResult | null> => {
    const selected = await open({
      directory: true,
      multiple: false,
      title: '选择要共享的文件夹',
    })
    if (!selected || Array.isArray(selected)) return null
    return api.addShare(selected)
  },
  pickFiles: async (): Promise<AddShareResult[]> => {
    const selected = await open({
      multiple: true,
      directory: false,
      title: '选择要共享的文件',
    })
    if (!selected) return []
    const paths = Array.isArray(selected) ? selected : [selected]
    if (!paths.length) return []
    return api.addShares(paths)
  },
  onLog: async (cb: (log: ServerLog) => void): Promise<UnlistenFn> => {
    return listen<ServerLog>('server-log', (e) => cb(e.payload))
  },
  onDragDrop: async (
    cb: (event: {
      type: 'enter' | 'over' | 'drop' | 'leave'
      paths?: string[]
      position?: { x: number; y: number }
    }) => void,
  ): Promise<UnlistenFn> => {
    const normalizePos = (position: unknown): { x: number; y: number } | undefined => {
      if (!position || typeof position !== 'object') return undefined
      const p = position as { x?: number; y?: number }
      if (typeof p.x === 'number' && typeof p.y === 'number') {
        return { x: p.x, y: p.y }
      }
      return undefined
    }

    let lastDropAt = 0
    const handle = (event: {
      payload: { type: string; paths?: string[]; position?: unknown }
    }) => {
      const payload = event.payload
      if (payload.type === 'drop') {
        const now = Date.now()
        if (now - lastDropAt < 400) return
        lastDropAt = now
        cb({
          type: 'drop',
          paths: payload.paths || [],
          position: normalizePos(payload.position),
        })
        return
      }
      if (payload.type === 'leave') {
        cb({ type: 'leave' })
        return
      }
      if (payload.type === 'over') {
        cb({ type: 'over', position: normalizePos(payload.position) })
        return
      }
      if (payload.type === 'enter') {
        cb({
          type: 'enter',
          paths: payload.paths || [],
          position: normalizePos(payload.position),
        })
      }
    }

    const offView = await getCurrentWebview().onDragDropEvent(handle)
    const offWin = await getCurrentWindow().onDragDropEvent(handle)
    return () => {
      offView()
      offWin()
    }
  },
}
