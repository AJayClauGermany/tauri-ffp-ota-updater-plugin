import { Channel, invoke } from '@tauri-apps/api/core'

/** Options used when checking for a frontend update. */
export interface CheckOptions {
  /** Request headers, also sent when downloading the bundle. */
  headers?: HeadersInit
  /** Timeout in milliseconds, applied to each request. */
  timeout?: number
}

/** Download progress events, identical to the updater plugin's. */
export type DownloadEvent =
  | { event: 'Started'; data: { contentLength?: number } }
  | { event: 'Progress'; data: { chunkLength: number } }
  | { event: 'Finished' }

interface UpdateMetadata {
  kind: 'frontend'
  currentVersion: string
  version: string
  downloaded: boolean
  date?: string
  body?: string
  rawJson: Record<string, unknown>
}

/** An available frontend update, returned by {@link check}. */
export class Update {
  /**
   * Distinguishes this from a full application update (`tauri-plugin-updater`)
   * when both are handled by the same code.
   */
  readonly kind = 'frontend' as const
  /** Version of the frontend currently being served. */
  currentVersion: string
  /** Version of the update. */
  version: string
  /**
   * Whether the bundle is already stored on this device, waiting for
   * {@link install} or the next launch. {@link download} then returns without
   * fetching it again.
   */
  downloaded: boolean
  date?: string
  /** Release notes. */
  body?: string
  /** The release manifest as the endpoint returned it. */
  rawJson: Record<string, unknown>

  constructor(metadata: UpdateMetadata) {
    this.currentVersion = metadata.currentVersion
    this.version = metadata.version
    this.downloaded = metadata.downloaded
    this.date = metadata.date
    this.body = metadata.body
    this.rawJson = metadata.rawJson
  }

  /**
   * Downloads, verifies and stores the frontend bundle. Without a following
   * {@link install} it takes effect the next time the app starts.
   *
   * Safe to call again for a bundle that is already stored: nothing is
   * fetched and only the `Finished` event is sent.
   */
  async download(onEvent?: (progress: DownloadEvent) => void): Promise<void> {
    await invoke('plugin:ffp-ota-updater|download', {
      version: this.version,
      onEvent: channel(onEvent),
    })
    this.downloaded = true
  }

  /**
   * Switches to the downloaded bundle now by reloading the app's webviews.
   * In-memory page state is lost, as with any reload. The new frontend has to
   * call {@link notifyReady} or it is reverted.
   */
  async install(): Promise<void> {
    await invoke('plugin:ffp-ota-updater|install', { version: this.version })
  }

  /** {@link download} followed by {@link install}. */
  async downloadAndInstall(
    onEvent?: (progress: DownloadEvent) => void
  ): Promise<void> {
    await invoke('plugin:ffp-ota-updater|download_and_install', {
      version: this.version,
      onEvent: channel(onEvent),
    })
  }
}

function channel(onEvent?: (progress: DownloadEvent) => void) {
  const channel = new Channel<DownloadEvent>()
  if (onEvent) {
    channel.onmessage = onEvent
  }
  return channel
}

/**
 * Checks for a frontend update. Resolves to `null` when there is none, or when
 * the published release needs a full application update instead.
 */
export async function check(options?: CheckOptions): Promise<Update | null> {
  const metadata = await invoke<UpdateMetadata | null>(
    'plugin:ffp-ota-updater|check',
    {
      headers: options?.headers
        ? Array.from(new Headers(options.headers).entries())
        : undefined,
      timeout: options?.timeout,
    }
  )
  return metadata ? new Update(metadata) : null
}

/**
 * Version of the frontend being served: the active downloaded bundle's, or
 * else the application's.
 */
export async function currentVersion(): Promise<string> {
  return await invoke('plugin:ffp-ota-updater|current_version')
}

/**
 * Deletes every downloaded bundle and goes back to the frontend built into the
 * application, reloading if a downloaded one was active.
 */
export async function reset(): Promise<void> {
  await invoke('plugin:ffp-ota-updater|reset')
}

/**
 * Confirms that the frontend being served works. Call it on every start, once
 * the app got far enough that it could update itself again.
 *
 * A downloaded bundle that was shown in two runs of the app (`trialAttempts`)
 * without confirming itself is reverted to the last confirmed one (or the
 * built-in frontend) and its version is never offered again. Disable with
 * `"rollback": false`.
 */
export async function notifyReady(): Promise<void> {
  await invoke('plugin:ffp-ota-updater|notify_ready')
}
