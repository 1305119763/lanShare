import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import QRCode from 'qrcode'
import { api, type ServerLog, type Settings, type ShareFolder } from './api'
import './styles.css'

function errMsg(e: unknown): string {
  if (typeof e === 'string') return e
  if (e instanceof Error) return e.message
  return '操作失败'
}

function filteredDirsHint(dirs?: string[]): string {
  if (!dirs?.length) return ''
  const shown = dirs.slice(0, 4).join('、')
  const rest = dirs.length > 4 ? ` 等 ${dirs.length} 个` : ''
  return `；依赖/缓存目录已自动过滤：${shown}${rest}（访问端可见提示）`
}

export default function App() {
  const [settings, setSettings] = useState<Settings | null>(null)
  const [running, setRunning] = useState(false)
  const [urls, setUrls] = useState<string[]>([])
  const [activeUrl, setActiveUrl] = useState('')
  const [qrDataUrl, setQrDataUrl] = useState('')
  const [logs, setLogs] = useState<ServerLog[]>([])
  const [showLogs, setShowLogs] = useState(false)
  const [showSettings, setShowSettings] = useState(false)
  const [busy, setBusy] = useState(false)
  const [toast, setToast] = useState('')
  const [hostname, setHostname] = useState('')
  const [renameId, setRenameId] = useState<string | null>(null)
  const [renameValue, setRenameValue] = useState('')
  const [dragging, setDragging] = useState(false)
  const [dropReady, setDropReady] = useState(false)

  const rightRef = useRef<HTMLElement | null>(null)
  const dropReadyRef = useRef(false)
  const draggingRef = useRef(false)

  const notify = useCallback((msg: string) => {
    setToast(msg)
    window.setTimeout(() => setToast(''), 2200)
  }, [])

  const refresh = useCallback(async () => {
    const [s, status, info] = await Promise.all([
      api.getSettings(),
      api.getServerStatus(),
      api.getAppInfo(),
    ])
    setSettings(s)
    setRunning(status.running)
    setUrls(status.urls)
    setActiveUrl((prev) => prev || status.urls[0] || '')
    setLogs(status.logs || [])
    setHostname(info.hostname)
  }, [])

  /** Tauri 给出的是物理像素，需换成逻辑像素再和 DOM rect 比较 */
  const toLogical = useCallback((pos?: { x: number; y: number }) => {
    if (!pos) return null
    const scale = window.devicePixelRatio || 1
    return { x: pos.x / scale, y: pos.y / scale }
  }, [])

  const hitRightPanel = useCallback(
    (pos?: { x: number; y: number }) => {
      const el = rightRef.current
      const logical = toLogical(pos)
      if (!el || !logical) return false
      const rect = el.getBoundingClientRect()
      return (
        logical.x >= rect.left &&
        logical.x <= rect.right &&
        logical.y >= rect.top &&
        logical.y <= rect.bottom
      )
    },
    [toLogical],
  )

  useEffect(() => {
    refresh()
    let offLog: (() => void) | undefined
    let offDrag: (() => void) | undefined

    api.onLog((log) => {
      setLogs((prev) => [log, ...prev].slice(0, 200))
    }).then((fn) => {
      offLog = fn
    })

    api
      .onDragDrop(async (ev) => {
        if (ev.type === 'enter' || ev.type === 'over') {
          draggingRef.current = true
          setDragging(true)
          const ready = hitRightPanel(ev.position)
          dropReadyRef.current = ready
          setDropReady(ready)
        } else if (ev.type === 'leave') {
          draggingRef.current = false
          setDragging(false)
          dropReadyRef.current = false
          setDropReady(false)
        } else if (ev.type === 'drop') {
          draggingRef.current = false
          setDragging(false)
          dropReadyRef.current = false
          setDropReady(false)

          const paths = (ev.paths || []).filter(Boolean)
          if (!paths.length) {
            notify('未获取到文件路径，请重试')
            return
          }
          try {
            const added = await api.addShares(paths)
            await refresh()
            const filteredTotal = added.reduce(
              (n, item) => n + (item.filteredDirs?.length ?? 0),
              0,
            )
            const hint =
              filteredTotal > 0
                ? '；部分项含已过滤的依赖/缓存目录（访问端会显示详情）'
                : ''
            notify(`已添加 ${added.length} 项共享${hint}`)
          } catch (e) {
            notify(errMsg(e))
          }
        }
      })
      .then((fn) => {
        offDrag = fn
      })

    return () => {
      offLog?.()
      offDrag?.()
    }
  }, [refresh, notify, hitRightPanel])

  useEffect(() => {
    if (!activeUrl) {
      setQrDataUrl('')
      return
    }
    QRCode.toDataURL(activeUrl, {
      width: 280,
      margin: 1,
      color: { dark: '#0b1220', light: '#ffffff' },
    }).then(setQrDataUrl)
  }, [activeUrl])

  const enabledCount = useMemo(
    () => settings?.folders.filter((f) => f.enabled).length ?? 0,
    [settings],
  )

  async function toggleServer() {
    if (!settings) return
    if (!running && settings.folders.length === 0) {
      notify('请先添加文件或文件夹')
      return
    }
    setBusy(true)
    try {
      if (running) {
        await api.stopServer()
        setRunning(false)
        notify('服务已停止')
      } else {
        const res = await api.startServer()
        setRunning(true)
        setUrls(res.urls)
        setActiveUrl(res.urls[0] || '')
        notify('服务已启动，可用手机扫码访问')
      }
    } catch (e) {
      notify(errMsg(e))
    } finally {
      setBusy(false)
      refresh()
    }
  }

  async function addFolder() {
    try {
      const folder = await api.pickFolder()
      if (folder) {
        await refresh()
        notify(`已添加文件夹「${folder.name}」${filteredDirsHint(folder.filteredDirs)}`)
      }
    } catch (e) {
      notify(errMsg(e))
    }
  }

  async function addFiles() {
    try {
      const items = await api.pickFiles()
      if (items.length) {
        await refresh()
        notify(`已添加 ${items.length} 个文件`)
      }
    } catch (e) {
      notify(errMsg(e))
    }
  }

  async function removeFolder(id: string) {
    await api.removeFolder(id)
    await refresh()
    notify('已移除共享')
  }

  async function toggleFolder(folder: ShareFolder) {
    await api.updateFolder(folder.id, { enabled: !folder.enabled })
    await refresh()
  }

  async function commitRename() {
    if (!renameId || !renameValue.trim()) {
      setRenameId(null)
      return
    }
    await api.updateFolder(renameId, { name: renameValue.trim() })
    setRenameId(null)
    await refresh()
  }

  async function savePort(port: number) {
    if (running) {
      notify('请先停止服务再修改端口')
      return
    }
    try {
      const next = await api.setSettings({ port })
      setSettings(next)
      notify('端口已保存')
    } catch (e) {
      notify(errMsg(e))
    }
  }

  async function saveUpload(allowUpload: boolean) {
    const next = await api.setSettings({ allowUpload })
    setSettings(next)
    if (running) {
      await api.stopServer()
      await api.startServer()
      await refresh()
    }
  }

  async function copyUrl() {
    if (!activeUrl) return
    await navigator.clipboard.writeText(activeUrl)
    notify('链接已复制')
  }

  if (!settings) {
    return (
      <div className="boot">
        <div className="boot-mark">瞬传</div>
        <div className="boot-sub">正在准备局域网快传…</div>
      </div>
    )
  }

  return (
    <div className="shell">
      <div className="glow glow-a" />
      <div className="glow glow-b" />

      <header className="topbar">
        <div className="brand-block">
          <div className="logo-mark">瞬</div>
          <div>
            <div className="brand-title">瞬传 LanShare</div>
            <div className="brand-sub">本机 · {hostname || '—'}</div>
          </div>
        </div>
        <div className={`status-chip ${running ? 'on' : 'off'}`}>
          <span className="dot" />
          {running ? '服务运行中' : '未启动'}
        </div>
      </header>

      <main className="layout">
        <section className="panel left-panel">
          <div className="panel-head">
            <h2>共享列表</h2>
            <div className="head-actions">
              <button className="btn ghost sm" onClick={addFiles}>
                + 文件
              </button>
              <button className="btn ghost sm" onClick={addFolder}>
                + 文件夹
              </button>
            </div>
          </div>

          <button
            className={`btn primary launch ${busy ? 'busy' : ''}`}
            onClick={toggleServer}
            disabled={busy}
          >
            {running ? '停止服务器' : '启动服务器'}
          </button>

          <div className="folder-list">
            {settings.folders.length === 0 && (
              <div className="empty-box">
                <p>还没有共享内容</p>
                <span>添加文件/文件夹，或拖到右侧区域</span>
              </div>
            )}
            {settings.folders.map((folder) => {
              const isFile = folder.kind === 'file'
              return (
                <article
                  key={folder.id}
                  className={`folder-card ${folder.enabled ? '' : 'disabled'}`}
                >
                  <div className="folder-main">
                    <button
                      className={`toggle ${folder.enabled ? 'on' : ''}`}
                      title={folder.enabled ? '暂停共享' : '启用共享'}
                      onClick={() => toggleFolder(folder)}
                    >
                      {folder.enabled ? '▶' : 'Ⅱ'}
                    </button>
                    <div className="folder-text">
                      {renameId === folder.id ? (
                        <input
                          className="rename-input"
                          autoFocus
                          value={renameValue}
                          onChange={(e) => setRenameValue(e.target.value)}
                          onBlur={commitRename}
                          onKeyDown={(e) => {
                            if (e.key === 'Enter') commitRename()
                            if (e.key === 'Escape') setRenameId(null)
                          }}
                        />
                      ) : (
                        <div className="folder-name">
                          <span className="kind-tag">{isFile ? '文件' : '文件夹'}</span>
                          {folder.name}
                        </div>
                      )}
                      <div className="folder-path" title={folder.path}>
                        {folder.path}
                      </div>
                    </div>
                  </div>
                  <div className="folder-actions">
                    <button
                      onClick={() => {
                        setRenameId(folder.id)
                        setRenameValue(folder.name)
                      }}
                    >
                      重命名
                    </button>
                    <button onClick={() => api.openPath(folder.path)}>打开</button>
                    <button className="danger" onClick={() => removeFolder(folder.id)}>
                      删除
                    </button>
                  </div>
                </article>
              )
            })}
          </div>

          <div className="left-foot">
            <span>
              已启用 {enabledCount}/{settings.folders.length} 项
            </span>
          </div>
        </section>

        <section
          className={`panel right-panel ${dragging ? 'drag-active' : ''} ${dropReady ? 'drop-ready' : ''}`}
          ref={rightRef}
        >
          {dragging && (
            <div className="drop-overlay">
              <div className="drop-card">
                <div className="drop-title">
                  {dropReady ? '松开以添加共享' : '拖到此区域添加'}
                </div>
                <div className="drop-sub">支持文件和文件夹</div>
              </div>
            </div>
          )}

          <div className="panel-head center">
            <h2>局域网访问</h2>
            <p className="hint">
              拖入文件/文件夹到此处即可添加共享；手机扫码或打开链接访问
            </p>
          </div>

          <div className={`qr-stage ${running ? 'live' : ''}`}>
            {running && qrDataUrl ? (
              <img className="qr" src={qrDataUrl} alt="访问二维码" />
            ) : (
              <div className="qr-placeholder">
                <div className="qr-ghost" />
                <p>启动服务后显示二维码</p>
              </div>
            )}
          </div>

          <div className="url-row">
            {urls.length > 1 && (
              <select
                className="url-select"
                value={activeUrl}
                onChange={(e) => setActiveUrl(e.target.value)}
              >
                {urls.map((u) => (
                  <option key={u} value={u}>
                    {u}
                  </option>
                ))}
              </select>
            )}
            {urls.length <= 1 && (
              <div className="url-box">{activeUrl || `http://—:${settings.port}`}</div>
            )}
            <button className="btn ghost" onClick={copyUrl} disabled={!running}>
              复制
            </button>
            <button
              className="btn ghost"
              disabled={!running || !activeUrl}
              onClick={() => api.openExternal(activeUrl)}
            >
              打开
            </button>
          </div>

          <div className="right-actions">
            <button className="btn ghost" onClick={() => setShowLogs((v) => !v)}>
              {showLogs ? '隐藏日志' : '显示日志'}
            </button>
            <button className="icon-btn" title="设置" onClick={() => setShowSettings(true)}>
              ⚙
            </button>
          </div>

          {showLogs && (
            <div className="log-panel">
              {logs.length === 0 && <div className="log-empty">暂无日志</div>}
              {logs.map((log) => (
                <div key={log.id} className={`log-line ${log.level}`}>
                  <span className="t">{log.time}</span>
                  <span className="m">{log.message}</span>
                </div>
              ))}
            </div>
          )}
        </section>
      </main>

      {showSettings && (
        <div className="modal-mask" onClick={() => setShowSettings(false)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <h3>设置</h3>
            <label className="field">
              <span>服务端口</span>
              <input
                type="number"
                min={1024}
                max={65535}
                defaultValue={settings.port}
                onBlur={(e) => {
                  const port = Number(e.target.value)
                  if (port >= 1024 && port <= 65535) savePort(port)
                }}
              />
            </label>
            <label className="field switch">
              <span>允许手机端上传文件</span>
              <input
                type="checkbox"
                checked={settings.allowUpload}
                onChange={(e) => saveUpload(e.target.checked)}
              />
            </label>
            <p className="modal-tip">
              仅限同一局域网访问。请勿在不可信网络长时间开放上传。
            </p>
            <button className="btn primary" onClick={() => setShowSettings(false)}>
              完成
            </button>
          </div>
        </div>
      )}

      {toast && <div className="toast">{toast}</div>}
    </div>
  )
}
