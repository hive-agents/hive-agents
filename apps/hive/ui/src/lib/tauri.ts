let invokeFn:
  | (<T>(command: string, args?: Record<string, unknown>) => Promise<T>)
  | null = null

export const isTauri = (): boolean => {
  return typeof globalThis !== 'undefined' && Boolean((globalThis as { isTauri?: boolean }).isTauri)
}

export const invoke = async <T>(
  command: string,
  args?: Record<string, unknown>,
): Promise<T> => {
  if (!isTauri()) {
    throw new Error('Tauri backend unavailable')
  }
  if (!invokeFn) {
    const mod = await import('@tauri-apps/api/core')
    invokeFn = mod.invoke
  }
  return invokeFn(command, args)
}

export const openDirectory = async (): Promise<string | null> => {
  if (!isTauri()) {
    return null
  }
  const { open } = await import('@tauri-apps/plugin-dialog')
  const selected = await open({ directory: true, multiple: false })
  if (Array.isArray(selected)) {
    return selected[0] ?? null
  }
  return selected ?? null
}

export const writeClipboard = async (value: string): Promise<void> => {
  if (navigator.clipboard?.writeText) {
    await navigator.clipboard.writeText(value)
    return
  }
  if (!isTauri()) {
    throw new Error('Clipboard unavailable')
  }
  const { writeText } = await import('@tauri-apps/plugin-clipboard-manager')
  await writeText(value)
}
