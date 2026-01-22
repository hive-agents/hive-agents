import { useEffect, useMemo, useState, type ReactNode } from 'react'
import beeImage from './assets/bee2.png'
import {
  connectProfile,
  disconnectSession,
  getLocalSettings,
  getStatus,
  logsTail,
  profileImport,
  profilesList,
  setLocalSettings,
} from './lib/api'
import { isTauri, openDirectory, writeClipboard } from './lib/tauri'
import type {
  LocalSettings,
  LocalSettingsPatch,
  LogLine,
  Platform,
  ProfileSummary,
  Status,
} from './lib/types'

const STATUS_REFRESH_MS = 5000
const DEFAULT_LOG_LINES = 200
const HELP_COMMAND = 'hive-core connect --host <public-host>'

type SettingsDraft = {
  mountpoint: string
  platform: Platform
  cacheDir: string
  cacheSizeMiB: string
}

const detectPlatform = (): Platform => {
  const ua = navigator.userAgent.toLowerCase()
  if (ua.includes('win')) return 'windows'
  if (ua.includes('mac')) return 'macos'
  return 'linux'
}

const makeDefaultSettings = (platform: Platform): SettingsDraft => ({
  mountpoint: '',
  platform,
  cacheDir: '',
  cacheSizeMiB: '10240',
})

const formatMaybe = (value: string | number | null | undefined): string => {
  if (value === null || value === undefined || value === '') {
    return '-'
  }
  return String(value)
}

const formatLogTime = (value: string): string => {
  const parsed = new Date(value)
  if (Number.isNaN(parsed.getTime())) {
    return value
  }
  return parsed.toLocaleTimeString()
}

const formatStateLabel = (value?: string | null): string => {
  if (!value) return 'idle'
  return value.replace(/_/g, ' ')
}

const errorMessage = (err: unknown, fallback: string): string => {
  if (err instanceof Error) return err.message
  if (typeof err === 'string') return err
  if (err && typeof err === 'object' && 'message' in err) {
    const message = (err as { message?: unknown }).message
    if (typeof message === 'string') return message
  }
  return fallback
}

const isSessionMissing = (message: string): boolean => {
  const lowered = message.toLowerCase()
  return (
    lowered.includes('session not found') ||
    lowered.includes('session not started')
  )
}

const resolvePort = (
  ports: Record<string, number> | undefined,
  name: string,
): string => {
  if (!ports) return '-'
  return ports[name] ? String(ports[name]) : '-'
}

const settingsFromLocal = (settings: LocalSettings): SettingsDraft => ({
  mountpoint: settings.mountpoint.path ?? '',
  platform: settings.mountpoint.platform,
  cacheDir: settings.cache.cache_dir ?? '',
  cacheSizeMiB: String(settings.cache.cache_size_mib ?? ''),
})

const App = () => {
  const [profiles, setProfiles] = useState<ProfileSummary[]>([])
  const [selectedProfileId, setSelectedProfileId] = useState<string | null>(
    null,
  )
  const [sessionId, setSessionId] = useState<string | null>(null)
  const [status, setStatus] = useState<Status | null>(null)
  const [logs, setLogs] = useState<LogLine[]>([])
  const [logLines, setLogLines] = useState(DEFAULT_LOG_LINES)
  const [autoRefresh, setAutoRefresh] = useState(true)
  const [importJson, setImportJson] = useState('')
  const [replaceExisting, setReplaceExisting] = useState(false)
  const [notice, setNotice] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState<string | null>(null)
  const [settingsDrafts, setSettingsDrafts] = useState<
    Record<string, SettingsDraft>
  >({})
  const [showHelpModal, setShowHelpModal] = useState(false)
  const [showStatusModal, setShowStatusModal] = useState(false)
  const [showLogsModal, setShowLogsModal] = useState(false)

  const activeSettings = useMemo(() => {
    if (!selectedProfileId) return null
    return settingsDrafts[selectedProfileId] ?? null
  }, [selectedProfileId, settingsDrafts])

  const selectionLocked = Boolean(sessionId)
  const isOnboarded = profiles.length > 0
  const isConnected = status?.state === 'mounted'
  const connectionTone = isConnected ? 'good' : 'bad'
  const connectionLabel = isConnected ? 'Connected' : 'Disconnected'

  const refreshProfiles = async () => {
    try {
      setBusy('Loading profiles...')
      const list = await profilesList()
      setProfiles(list)
      setSelectedProfileId((current) => {
        if (current && list.some((profile) => profile.profile_id === current)) {
          return current
        }
        return list[0]?.profile_id ?? null
      })
      setError(null)
    } catch (err) {
      setError(errorMessage(err, 'Failed to load profiles'))
    } finally {
      setBusy(null)
    }
  }

  useEffect(() => {
    refreshProfiles()
  }, [])

  useEffect(() => {
    if (!selectedProfileId) return
    let cancelled = false
    const loadSettings = async () => {
      try {
        const settings = await getLocalSettings(selectedProfileId)
        if (!cancelled) {
          setSettingsDrafts((current) => ({
            ...current,
            [selectedProfileId]: settingsFromLocal(settings),
          }))
          setError(null)
        }
      } catch (err) {
        if (!cancelled) {
          setSettingsDrafts((current) => ({
            ...current,
            [selectedProfileId]:
              current[selectedProfileId] ??
              makeDefaultSettings(detectPlatform()),
          }))
          setError(errorMessage(err, 'Failed to load local settings'))
        }
      }
    }
    loadSettings()
    return () => {
      cancelled = true
    }
  }, [selectedProfileId])

  useEffect(() => {
    if (!sessionId || !autoRefresh) return
    let cancelled = false
    const tick = async () => {
      try {
        const [nextStatus, nextLogs] = await Promise.all([
          getStatus(sessionId),
          logsTail(sessionId, logLines),
        ])
        if (!cancelled) {
          setStatus(nextStatus)
          setLogs(nextLogs)
        }
    } catch (err) {
      if (!cancelled) {
        const message = errorMessage(err, 'Refresh failed')
        if (isSessionMissing(message)) {
          resetSession('Session ended')
        } else {
          setError(message)
        }
      }
    }
  }
    tick()
    const timer = window.setInterval(tick, STATUS_REFRESH_MS)
    return () => {
      cancelled = true
      window.clearInterval(timer)
    }
  }, [sessionId, autoRefresh, logLines])

  const refreshStatus = async () => {
    if (!sessionId) return
    try {
      const nextStatus = await getStatus(sessionId)
      setStatus(nextStatus)
    } catch (err) {
      const message = errorMessage(err, 'Status refresh failed')
      if (isSessionMissing(message)) {
        resetSession('Session ended')
      } else {
        setError(message)
      }
    }
  }

  const refreshLogs = async () => {
    if (!sessionId) return
    try {
      const nextLogs = await logsTail(sessionId, logLines)
      setLogs(nextLogs)
    } catch (err) {
      const message = errorMessage(err, 'Log fetch failed')
      if (isSessionMissing(message)) {
        resetSession('Session ended')
      } else {
        setError(message)
      }
    }
  }

  const handleImport = async () => {
    if (!importJson.trim()) return
    try {
      JSON.parse(importJson)
    } catch (err) {
      setError('Profile JSON is invalid')
      return
    }
    try {
      setBusy('Importing profile...')
      const profileId = await profileImport(importJson, replaceExisting)
      const list = await profilesList()
      setProfiles(list)
      setSelectedProfileId(profileId)
      setImportJson('')
      setReplaceExisting(false)
      setNotice('Profile imported')
    } catch (err) {
      setError(errorMessage(err, 'Profile import failed'))
    } finally {
      setBusy(null)
    }
  }

  const handleConnect = async () => {
    if (!selectedProfileId) return
    if (sessionId) {
      setNotice('Disconnect the active session first')
      return
    }
    try {
      setBusy('Connecting...')
      const id = await connectProfile(selectedProfileId)
      setSessionId(id)
      setNotice('Session started')
      const nextStatus = await getStatus(id)
      setStatus(nextStatus)
    } catch (err) {
      setError(errorMessage(err, 'Connect failed'))
    } finally {
      setBusy(null)
    }
  }

  const handleDisconnect = async () => {
    if (!sessionId) return
    try {
      setBusy('Disconnecting...')
      await disconnectSession(sessionId)
      setSessionId(null)
      setStatus(null)
      setLogs([])
      setNotice('Disconnected')
    } catch (err) {
      const message = errorMessage(err, 'Disconnect failed')
      if (isSessionMissing(message)) {
        resetSession('Session already stopped')
      } else {
        setError(message)
      }
    } finally {
      setBusy(null)
    }
  }

  const updateDraft = (patch: Partial<SettingsDraft>) => {
    if (!selectedProfileId) return
    setSettingsDrafts((current) => {
      const existing =
        current[selectedProfileId] ?? makeDefaultSettings(detectPlatform())
      return {
        ...current,
        [selectedProfileId]: {
          ...existing,
          ...patch,
        },
      }
    })
  }

  const handleBrowse = async (field: 'mountpoint' | 'cacheDir') => {
    try {
      if (selectionLocked) {
        setNotice('Disconnect to edit local settings')
        return
      }
      if (!isTauri()) {
        setNotice('Directory picker is available in the desktop app')
        return
      }
      const selected = await openDirectory()
      if (!selected) return
      if (field === 'mountpoint') {
        updateDraft({ mountpoint: selected })
      } else {
        updateDraft({ cacheDir: selected })
      }
    } catch (err) {
      setError(errorMessage(err, 'Picker failed'))
    }
  }

  const handleSaveSettings = async () => {
    if (!selectedProfileId || !activeSettings) return
    if (selectionLocked) {
      setNotice('Disconnect to update local settings')
      return
    }
    const patch: LocalSettingsPatch = {}
    if (activeSettings.mountpoint.trim()) {
      patch.mountpoint = {
        path: activeSettings.mountpoint.trim(),
        platform: activeSettings.platform,
      }
    }
    const cachePatch: NonNullable<LocalSettingsPatch['cache']> = {}
    if (activeSettings.cacheDir.trim()) {
      cachePatch.cache_dir = activeSettings.cacheDir.trim()
    }
    if (activeSettings.cacheSizeMiB.trim()) {
      const parsed = Number(activeSettings.cacheSizeMiB)
      if (!Number.isNaN(parsed)) {
        cachePatch.cache_size_mib = parsed
      }
    }
    if (Object.keys(cachePatch).length) {
      patch.cache = cachePatch
    }
    if (!Object.keys(patch).length) {
      setError('No settings to update')
      return
    }
    try {
      setBusy('Saving settings...')
      await setLocalSettings(selectedProfileId, patch)
      setNotice('Local settings saved')
    } catch (err) {
      setError(errorMessage(err, 'Settings update failed'))
    } finally {
      setBusy(null)
    }
  }

  const handleSelectProfile = (profileId: string) => {
    if (selectionLocked) {
      setNotice('Disconnect to switch profiles')
      return
    }
    setSelectedProfileId(profileId)
  }

  const copyCommand = async (value: string): Promise<boolean> => {
    try {
      await writeClipboard(value)
      return true
    } catch (err) {
      setError(errorMessage(err, 'Copy failed'))
      return false
    }
  }

  const handleOpenLogs = () => {
    if (!sessionId) {
      setNotice('No active session')
      return
    }
    setShowLogsModal(true)
  }

  const resetSession = (message?: string) => {
    setSessionId(null)
    setStatus(null)
    setLogs([])
    if (message) {
      setNotice(message)
    }
  }

  const clearNotice = () => setNotice(null)
  const clearError = () => setError(null)

  return (
    <div className="app">
      <BackgroundArt />
      <div className="shell">
        {isOnboarded && (
          <header className="topbar">
            <div className="topbar__actions">
              <div className="select-group">
                <label className="select-label" htmlFor="profile-select">
                  Profile
                </label>
                <select
                  id="profile-select"
                  className="select"
                  value={selectedProfileId ?? ''}
                  onChange={(event) => handleSelectProfile(event.target.value)}
                  disabled={selectionLocked}
                >
                  {profiles.map((profile) => (
                    <option key={profile.profile_id} value={profile.profile_id}>
                      {profile.display_name}
                    </option>
                  ))}
                </select>
              </div>
              <button
                className={`btn ${sessionId ? 'btn--ghost' : 'btn--primary'}`}
                onClick={sessionId ? handleDisconnect : handleConnect}
                disabled={!selectedProfileId}
              >
                {sessionId ? 'Disconnect' : 'Connect'}
              </button>
            </div>
          </header>
        )}

        <main className="screen">
          {!isOnboarded ? (
            <section className="onboard">
              <div className="panel panel--onboard">
                <h1 className="onboard__title">Connect Hive</h1>
                <textarea
                  className="input input--area"
                  placeholder='{ "profile_version": 1, ... }'
                  value={importJson}
                  onChange={(event) => setImportJson(event.target.value)}
                  rows={8}
                />
                <div className="panel__footer panel__footer--split">
                  <label className="toggle">
                    <input
                      type="checkbox"
                      checked={replaceExisting}
                      onChange={(event) =>
                        setReplaceExisting(event.target.checked)
                      }
                    />
                    Replace existing device name
                  </label>
                  <div className="panel__actions">
                    <button
                      className="btn btn--ghost"
                      onClick={() => setShowHelpModal(true)}
                    >
                      Need help?
                    </button>
                    <button
                      className="btn btn--primary"
                      onClick={handleImport}
                      disabled={!importJson.trim()}
                    >
                      Connect
                    </button>
                  </div>
                </div>
              </div>
            </section>
          ) : (
            <section className="workspace">
              <div className="panel panel--settings">
                <div className="panel__header">
                  <div>
                    <h2>Local settings</h2>
                    <p className="muted">
                      Choose where Hive mounts and caches your files.
                    </p>
                  </div>
                </div>
                <div className="form">
                  <label className="field">
                    <span>Mountpoint</span>
                    <div className="field__row">
                      <input
                        className="input"
                        value={activeSettings?.mountpoint ?? ''}
                        onChange={(event) =>
                          updateDraft({ mountpoint: event.target.value })
                        }
                        placeholder="/Users/alex/hive"
                        disabled={selectionLocked}
                      />
                      <button
                        className="btn btn--ghost"
                        type="button"
                        onClick={() => handleBrowse('mountpoint')}
                        disabled={selectionLocked}
                      >
                        Browse
                      </button>
                    </div>
                  </label>

                  <label className="field">
                    <span>Platform</span>
                    <select
                      className="input"
                      value={activeSettings?.platform ?? detectPlatform()}
                      onChange={(event) =>
                        updateDraft({ platform: event.target.value as Platform })
                      }
                      disabled={selectionLocked}
                    >
                      <option value="macos">macOS</option>
                      <option value="linux">Linux</option>
                      <option value="windows">Windows</option>
                    </select>
                  </label>

                  <label className="field">
                    <span>Cache directory</span>
                    <div className="field__row">
                      <input
                        className="input"
                        value={activeSettings?.cacheDir ?? ''}
                        onChange={(event) =>
                          updateDraft({ cacheDir: event.target.value })
                        }
                        placeholder="/Users/alex/Library/Application Support/Hive/cache"
                        disabled={selectionLocked}
                      />
                      <button
                        className="btn btn--ghost"
                        type="button"
                        onClick={() => handleBrowse('cacheDir')}
                        disabled={selectionLocked}
                      >
                        Browse
                      </button>
                    </div>
                  </label>

                  <label className="field">
                    <span>Cache size (MiB)</span>
                    <input
                      className="input"
                      type="number"
                      min={512}
                      value={activeSettings?.cacheSizeMiB ?? '10240'}
                      onChange={(event) =>
                        updateDraft({ cacheSizeMiB: event.target.value })
                      }
                      disabled={selectionLocked}
                    />
                  </label>
                </div>
                <div className="panel__footer">
                  <button
                    className="btn btn--primary"
                    onClick={handleSaveSettings}
                    disabled={!selectedProfileId || selectionLocked}
                  >
                    Save settings
                  </button>
                </div>
              </div>

              <div className="panel panel--status">
                <div className="panel__header">
                  <div>
                    <h2>Connection</h2>
                    <p className="muted">
                      {sessionId
                        ? 'Tunnel and mount status for this device.'
                        : 'Connect to start mounting your hive.'}
                    </p>
                  </div>
                  <button
                    className={`status-indicator status-indicator--${connectionTone}`}
                    type="button"
                    onClick={() => setShowStatusModal(true)}
                  >
                    <span className="status-indicator__dot" />
                    <span className="status-indicator__label">
                      {connectionLabel}
                    </span>
                  </button>
                </div>

                <div className="panel__footer panel__footer--split">
                  <button
                    className="btn btn--ghost"
                    onClick={refreshStatus}
                    disabled={!sessionId}
                  >
                    Refresh status
                  </button>
                  <button
                    className={`btn ${sessionId ? 'btn--ghost' : 'btn--primary'}`}
                    onClick={sessionId ? handleDisconnect : handleConnect}
                    disabled={!selectedProfileId}
                  >
                    {sessionId ? 'Disconnect' : 'Connect'}
                  </button>
                </div>
              </div>
            </section>
          )}
        </main>
      </div>

      {isOnboarded && (
        <button
          className="logs-fab"
          type="button"
          onClick={handleOpenLogs}
          title="View logs"
          disabled={!sessionId}
        >
          <span className="logs-fab__icon" aria-hidden="true">
            <LogIcon />
          </span>
        </button>
      )}

      {showHelpModal && (
        <Modal title="Need help?" onClose={() => setShowHelpModal(false)}>
          <p className="modal__text">
            To connect, run command on server, then copy connection info here.
          </p>
          <CommandBlock
            command={HELP_COMMAND}
            onCopy={() => copyCommand(HELP_COMMAND)}
          />
        </Modal>
      )}

      {showStatusModal && (
        <Modal title="Session status" onClose={() => setShowStatusModal(false)}>
          <div className="status-grid">
            <div>
              <span className="label">Session</span>
              <p>{formatMaybe(sessionId)}</p>
            </div>
            <div>
              <span className="label">State</span>
              <p>{formatStateLabel(status?.state)}</p>
            </div>
            <div>
              <span className="label">Mountpoint</span>
              <p>{formatMaybe(status?.mountpoint)}</p>
            </div>
            <div>
              <span className="label">Postgres port</span>
              <p>{resolvePort(status?.local_ports, 'postgres')}</p>
            </div>
            <div>
              <span className="label">S3 port</span>
              <p>{resolvePort(status?.local_ports, 's3')}</p>
            </div>
            <div>
              <span className="label">Last error</span>
              <p>{formatMaybe(status?.last_error)}</p>
            </div>
          </div>
        </Modal>
      )}

      {showLogsModal && (
        <Modal
          title="Session logs"
          onClose={() => setShowLogsModal(false)}
          wide
        >
          <div className="logs-controls">
            <label className="toggle">
              <input
                type="checkbox"
                checked={autoRefresh}
                onChange={(event) => setAutoRefresh(event.target.checked)}
              />
              Auto-refresh
            </label>
            <label className="field field--inline">
              <span>Lines</span>
              <input
                className="input input--compact"
                type="number"
                min={50}
                max={1000}
                value={logLines}
                onChange={(event) => {
                  const next = Number(event.target.value)
                  setLogLines(Number.isNaN(next) ? DEFAULT_LOG_LINES : next)
                }}
              />
            </label>
            <button className="btn btn--ghost" onClick={refreshLogs}>
              Fetch
            </button>
          </div>
          <div className="logs">
            {logs.length === 0 && (
              <div className="empty">No logs yet.</div>
            )}
            {logs.map((line, index) => (
              <div key={`${line.timestamp}-${index}`} className="log-line">
                <span className={`log-level log-${line.level}`}>
                  {line.level.toUpperCase()}
                </span>
                <span className="log-source">{line.source}</span>
                <span className="log-time">{formatLogTime(line.timestamp)}</span>
                <span className="log-message">{line.message}</span>
              </div>
            ))}
          </div>
        </Modal>
      )}

      {(notice || error || busy) && (
        <div className="toasts">
          {busy && (
            <div className="toast toast--busy" onClick={clearNotice}>
              {busy}
            </div>
          )}
          {notice && (
            <div className="toast" onClick={clearNotice}>
              {notice}
            </div>
          )}
          {error && (
            <div className="toast toast--error" onClick={clearError}>
              {error}
            </div>
          )}
        </div>
      )}
    </div>
  )
}

const LogIcon = () => (
  <svg viewBox="0 0 24 24" width="24" height="24" fill="none">
    <rect
      x="4"
      y="5"
      width="16"
      height="14"
      rx="3"
      stroke="currentColor"
      strokeWidth="1.5"
    />
    <path
      d="M8 9H16"
      stroke="currentColor"
      strokeWidth="1.5"
      strokeLinecap="round"
    />
    <path
      d="M8 12.5H14"
      stroke="currentColor"
      strokeWidth="1.5"
      strokeLinecap="round"
    />
    <circle cx="16.5" cy="15.5" r="1" fill="currentColor" />
  </svg>
)

const CloseIcon = () => (
  <svg viewBox="0 0 24 24" width="20" height="20" fill="none">
    <path
      d="M6 6L18 18"
      stroke="currentColor"
      strokeWidth="1.5"
      strokeLinecap="round"
    />
    <path
      d="M18 6L6 18"
      stroke="currentColor"
      strokeWidth="1.5"
      strokeLinecap="round"
    />
  </svg>
)

const CopyIcon = () => (
  <svg viewBox="0 0 24 24" width="20" height="20" fill="none">
    <rect
      x="9"
      y="9"
      width="10"
      height="10"
      rx="2"
      stroke="currentColor"
      strokeWidth="1.5"
    />
    <rect
      x="5"
      y="5"
      width="10"
      height="10"
      rx="2"
      stroke="currentColor"
      strokeWidth="1.5"
      opacity="0.6"
    />
  </svg>
)

const CheckIcon = () => (
  <svg viewBox="0 0 24 24" width="20" height="20" fill="none">
    <path
      d="M6 12L10 16L18 8"
      stroke="currentColor"
      strokeWidth="1.7"
      strokeLinecap="round"
      strokeLinejoin="round"
    />
  </svg>
)

type ModalProps = {
  title: string
  onClose: () => void
  children: ReactNode
  wide?: boolean
}

const Modal = ({ title, onClose, children, wide }: ModalProps) => (
  <div className="modal">
    <div className="modal__backdrop" onClick={onClose} />
    <div className={`modal__panel${wide ? ' modal__panel--wide' : ''}`}>
      <div className="modal__header">
        <h3>{title}</h3>
        <button
          className="btn btn--ghost icon-button"
          onClick={onClose}
          aria-label="Close"
        >
          <CloseIcon />
        </button>
      </div>
      {children}
    </div>
  </div>
)

type CommandBlockProps = {
  command: string
  onCopy: () => Promise<boolean>
}

const CommandBlock = ({ command, onCopy }: CommandBlockProps) => {
  const [copied, setCopied] = useState(false)
  const handleClick = async () => {
    const ok = await onCopy()
    if (ok) {
      setCopied(true)
    }
  }
  return (
    <div className="command">
      <code>{command}</code>
      <button
        className={`btn btn--ghost icon-button${copied ? ' icon-button--success' : ''}`}
        onClick={handleClick}
        aria-label={copied ? 'Copied' : 'Copy'}
      >
        {copied ? <CheckIcon /> : <CopyIcon />}
      </button>
    </div>
  )
}

const BackgroundArt = () => (
  <div className="background" aria-hidden="true">
    <BeeTrails />
    <div className="background__grain" />
  </div>
)

const BeeTrails = () => {
  const path1 =
    'M-50,80 C100,20 150,180 300,100 C450,20 400,200 550,120 C700,40 750,180 900,80 C1050,-20 1100,160 1250,100'
  const path2 =
    'M-50,280 C50,180 150,350 300,280 C450,210 400,400 550,320 C700,240 800,380 950,300 C1100,220 1150,350 1250,280'
  const path3 =
    'M-50,420 C100,350 200,480 350,400 C500,320 550,500 700,420 C850,340 900,480 1050,400 C1200,320 1180,460 1250,420'

  return (
    <div className="bees">
      <svg
        className="bees__paths"
        viewBox="0 0 1200 500"
        preserveAspectRatio="xMidYMid slice"
      >
        <path
          d={path1}
          fill="none"
          stroke="var(--color-hexagon-stroke)"
          strokeWidth="2"
          strokeDasharray="6 6"
          opacity="0.3"
        />
        <path
          d={path2}
          fill="none"
          stroke="var(--color-hexagon-stroke)"
          strokeWidth="2"
          strokeDasharray="6 6"
          opacity="0.3"
        />
        <path
          d={path3}
          fill="none"
          stroke="var(--color-hexagon-stroke)"
          strokeWidth="2"
          strokeDasharray="6 6"
          opacity="0.3"
        />
        <defs>
          <path id="hiveBeePath1" d={path1} />
          <path id="hiveBeePath2" d={path2} />
          <path id="hiveBeePath3" d={path3} />
        </defs>
      </svg>
      <svg
        className="bees__flies"
        viewBox="0 0 1200 500"
        preserveAspectRatio="xMidYMid slice"
      >
        <g className="bee bee--one">
          <animateMotion
            dur="16s"
            repeatCount="indefinite"
            rotate="auto"
            calcMode="spline"
            keySplines="0.4 0 0.2 1; 0.4 0 0.2 1; 0.4 0 0.2 1; 0.4 0 0.2 1"
            keyTimes="0; 0.25; 0.5; 0.75; 1"
          >
            <mpath href="#hiveBeePath1" />
          </animateMotion>
          <g transform="rotate(90)">
            <image href={beeImage} x="-18" y="-18" width="36" height="36" />
          </g>
        </g>
        <g className="bee bee--two">
          <animateMotion
            dur="20s"
            repeatCount="indefinite"
            rotate="auto"
            begin="-6s"
            calcMode="spline"
            keySplines="0.4 0 0.2 1; 0.4 0 0.2 1; 0.4 0 0.2 1; 0.4 0 0.2 1"
            keyTimes="0; 0.25; 0.5; 0.75; 1"
          >
            <mpath href="#hiveBeePath2" />
          </animateMotion>
          <g transform="rotate(90)">
            <image href={beeImage} x="-16" y="-16" width="32" height="32" />
          </g>
        </g>
        <g className="bee bee--three">
          <animateMotion
            dur="18s"
            repeatCount="indefinite"
            rotate="auto"
            begin="-12s"
            calcMode="spline"
            keySplines="0.4 0 0.2 1; 0.4 0 0.2 1; 0.4 0 0.2 1; 0.4 0 0.2 1"
            keyTimes="0; 0.25; 0.5; 0.75; 1"
          >
            <mpath href="#hiveBeePath3" />
          </animateMotion>
          <g transform="rotate(90)">
            <image href={beeImage} x="-14" y="-14" width="28" height="28" />
          </g>
        </g>
      </svg>
    </div>
  )
}

export default App
