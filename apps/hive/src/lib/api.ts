import { invoke } from './tauri'
import type {
  LocalSettings,
  LocalSettingsPatch,
  LogLine,
  ProfileSummary,
  Status,
} from './types'

export const profilesList = async (): Promise<ProfileSummary[]> => {
  return invoke<ProfileSummary[]>('profiles_list')
}

export const profileImport = async (json: string): Promise<string> => {
  return invoke<string>('profile_import', { json })
}

export const profileExport = async (profileId: string): Promise<string> => {
  return invoke<string>('profile_export', { profile_id: profileId })
}

export const getLocalSettings = async (
  profileId: string,
): Promise<LocalSettings> => {
  return invoke<LocalSettings>('get_local_settings', { profile_id: profileId })
}

export const connectProfile = async (profileId: string): Promise<string> => {
  return invoke<string>('connect', { profile_id: profileId })
}

export const disconnectSession = async (sessionId: string): Promise<void> => {
  return invoke<void>('disconnect', { session_id: sessionId })
}

export const getStatus = async (sessionId: string): Promise<Status> => {
  return invoke<Status>('status', { session_id: sessionId })
}

export const logsTail = async (
  sessionId: string,
  limit: number,
): Promise<LogLine[]> => {
  return invoke<LogLine[]>('logs_tail', { session_id: sessionId, n: limit })
}

export const setLocalSettings = async (
  profileId: string,
  patch: LocalSettingsPatch,
): Promise<void> => {
  return invoke<void>('set_local_settings', { profile_id: profileId, patch })
}
