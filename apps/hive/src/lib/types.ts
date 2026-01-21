export type ProfileSummary = {
  profile_id: string
  display_name: string
  created_at: string
}

export type ProfileImportResult = {
  profile_id: string
  needs_authorization: boolean
  device_public_key?: string | null
}

export type Platform = 'macos' | 'linux' | 'windows'

export type Status = {
  state: string
  mountpoint?: string | null
  last_error?: string | null
  local_ports?: Record<string, number>
}

export type LogLine = {
  timestamp: string
  level: 'info' | 'warn' | 'error'
  source: 'supervisor' | 'tunnel' | 'mount'
  message: string
}

export type LocalSettings = {
  profile_id: string
  mountpoint: {
    path: string
    platform: Platform
  }
  cache: {
    cache_dir: string
    cache_size_mib: number
  }
  runtime?: {
    last_local_postgres_port?: number | null
    last_local_s3_port?: number | null
    last_connected_at?: string | null
  }
}

export type LocalSettingsPatch = {
  mountpoint?: {
    path?: string
    platform?: Platform
  }
  cache?: {
    cache_dir?: string
    cache_size_mib?: number
  }
}
