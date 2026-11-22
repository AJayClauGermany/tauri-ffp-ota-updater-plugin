import { Channel, invoke } from '@tauri-apps/api/core'

/** Options used when checking for a frontend update. */
export interface CheckOptions {
  /**
   * Headers for the manifest request. Also sent with the bundle download when
   * the bundle is on the same origin as the endpoint. They never leave that
   * origin: the manifest is not signed, so they follow neither it nor a
   * redirect to another host.
   */
  headers?: HeadersInit
  /** Timeout in milliseconds, applied to each request. */
  timeout?: number
  /** Proxy URL for the manifest request and the bundle download. */
  proxy?: string
}

/**
 * What went wrong, in a form that is safe to branch on. `network` means no
 * endpoint or download host could be reached, which is what an offline device
 * gets. More kinds may be added.
 */
export type OtaUpdateErrorKind =
  | 'network'
  | 'releaseNotFound'
  | 'signature'
  | 'missingSignedVersion'
  | 'signedVersionMismatch'
  | 'invalidBundle'
  | 'tooLarge'
  | 'incompatible'
  | 'rejected'
  | 'noPendingUpdate'
  | 'notDownloaded'
  | 'notNewer'
  | 'inUse'
  | 'emptyEndpoints'
  | 'emptyPubkey'
  | 'cannotServe'
  | 'insecureTransportProtocol'
  | 'invalidHeader'
  | 'invalidUrl'
  | 'invalidVersion'
  | 'io'
  | 'json'
  | 'tauri'
  | 'unknown'

/** The error every function of this module rejects with. */
export class OtaUpdateError extends Error {
  readonly kind: OtaUpdateErrorKind

  constructor(kind: OtaUpdateErrorKind, message: string) {
    super(message)
    this.name = 'OtaUpdateError'
    this.kind = kind
  }
}

/** Where the updater stands, from {@link status}. */
export interface Status {
  /** Version of the application, i.e. of the frontend built into it. */
  binaryVersion: string
  /** Version of the frontend being served. */
  currentVersion: string
  /**
   * `false` while the frontend being served is a downloaded bundle that has
   * not called {@link notifyReady} yet, and is reverted if it never does.
   */
  confirmed: boolean
  /**
   * A downloaded bundle that is newer than the one being served. The next
   * launch switches to it; {@link install} does so now and {@link discard}
   * deletes it.
   */
  staged?: string
  /**
   * Versions that failed their trial or were rolled back, oldest first. They
   * are not offered again.
   */
  rejected: string[]
}

async function call<T>(
  command: string,
  args?: Record<string, unknown>
): Promise<T> {
  try {
    return await invoke<T>(`plugin:ffp-ota-updater|${command}`, args)
  } catch (error) {
    if (
      typeof error === 'object' &&
      error !== null &&
      'kind' in error &&
      'message' in error
    ) {
      throw new OtaUpdateError(
        error.kind as OtaUpdateErrorKind,
        String(error.message)
      )
    }
    // not from the plugin: a denied permission, for instance
    throw new OtaUpdateError('unknown', String(error))
  }
}

/** Download progress events, identical to the updater plugin's. */
export type DownloadEvent =
  | { event: 'Started'; data: { contentLength?: number } }
  | { event: 'Progress'; data: { chunkLength: number } }
  | { event: 'Finished' }

interface UpdateMetadata {
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
    await call('download', {
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
    await install(this.version)
  }

  /** {@link download} followed by {@link install}. */
  async downloadAndInstall(
    onEvent?: (progress: DownloadEvent) => void
  ): Promise<void> {
    await call('download_and_install', {
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
  let headers: [string, string][] | undefined
  try {
    headers = options?.headers
      ? Array.from(new Headers(options.headers).entries())
      : undefined
  } catch (error) {
    // `Headers` throws a `TypeError` for a name or value it cannot send
    throw new OtaUpdateError('invalidHeader', String(error))
  }
  const metadata = await call<UpdateMetadata | null>('check', {
    headers,
    timeout: options?.timeout,
    proxy: options?.proxy,
  })
  return metadata ? new Update(metadata) : null
}

/**
 * Version of the frontend being served: the active downloaded bundle's, or
 * else the application's.
 */
export async function currentVersion(): Promise<string> {
  return await call('current_version')
}

/**
 * What is being served and whether it is confirmed, what is staged for the
 * next launch and which versions were rolled back. Makes no request, so it
 * works offline.
 */
export async function status(): Promise<Status> {
  return await call('status')
}

/**
 * Switches to the stored bundle `version` now by reloading the app's
 * webviews, like {@link Update.install}, but without a {@link check}: a bundle
 * that {@link status} reports as `staged` can be applied offline.
 *
 * Rejects with kind `cannotServe` where no downloaded frontend can be shown:
 * under `tauri dev`, when the app did not call `wrap_assets`, or when its
 * `frontendDist` is a remote URL.
 */
export async function install(version: string): Promise<void> {
  await call('install', { version })
}

/**
 * Deletes the stored bundle `version` so that the next launch does not switch
 * to it, for an update the user declined. The version can be downloaded
 * again. Rejects with kind `inUse` for the bundle being served and for the
 * confirmed one kept to fall back on.
 */
export async function discard(version: string): Promise<void> {
  await call('discard', { version })
}

/**
 * Abandons the frontend being served: its version is rejected for good and
 * the app reloads into the last confirmed bundle, or else the built-in
 * frontend. A newer bundle that was downloaded but never shown is kept, and
 * is tried at the next launch. For a frontend that notices it is broken. Does
 * nothing when the built-in frontend is being served.
 *
 * Not part of the default permission set: grant
 * `ffp-ota-updater:allow-rollback`.
 */
export async function rollback(): Promise<void> {
  await call('rollback')
}

/**
 * Deletes every downloaded bundle and goes back to the frontend built into the
 * application, reloading if a downloaded one was active.
 *
 * Not part of the default permission set: grant
 * `ffp-ota-updater:allow-reset`.
 */
export async function reset(): Promise<void> {
  await call('reset')
}

/**
 * Confirms that the frontend being served works. Call it on every start, once
 * the app got far enough that it could update itself again.
 *
 * Only a page that was loaded from the bundle on trial confirms it: while
 * {@link install} reloads the windows one after the other, a window that still
 * shows the previous frontend cannot vouch for the new one.
 *
 * A downloaded bundle that was shown in two runs of the app (`trialAttempts`)
 * without confirming itself is reverted to the last confirmed one (or the
 * built-in frontend) and its version is never offered again. Disable with
 * `"rollback": false`.
 */
export async function notifyReady(): Promise<void> {
  await call('notify_ready')
}
