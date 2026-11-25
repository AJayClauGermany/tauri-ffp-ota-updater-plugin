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
  /**
   * Timeout in milliseconds, applied to each request: the manifest request,
   * and the bundle download unless {@link Update.download} sets its own.
   */
  timeout?: number
  /** Proxy URL for the manifest request and the bundle download. */
  proxy?: string
}

/**
 * Options for one {@link Update.download}, as in the updater plugin. What is
 * left out is what {@link check} used.
 */
export interface DownloadOptions {
  /**
   * Headers for the bundle download, instead of those given to {@link check}.
   * Like those, they are only sent when the bundle is on the endpoint's
   * origin.
   */
  headers?: HeadersInit
  /**
   * Timeout in milliseconds for the download, instead of the one given to
   * {@link check}, or else 30 minutes.
   */
  timeout?: number
}

/**
 * What went wrong, in a form that is safe to branch on. `network` means no
 * endpoint or download host could be reached, which is what an offline device
 * gets. More kinds may be added.
 */
export type OtaUpdateErrorKind =
  | 'network'
  | 'releaseNotFound'
  | 'invalidSignature'
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
  | 'invalidConfig'
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
   * A downloaded bundle that is newer than the one being served.
   * {@link install} switches to it now and {@link discard} deletes it. The
   * next launch switches to it by itself, unless the app is configured with
   * `requireApproval`: then it waits, across restarts, for {@link approve}.
   */
  staged?: string
  /**
   * The downloaded bundle that was approved with {@link approve} and is newer
   * than the one being served: with `requireApproval`, what the next launch
   * switches to. It can be older than `staged`, when a newer bundle was
   * downloaded since.
   */
  approved?: string
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
   * {@link install}, the next launch or, with `requireApproval`,
   * {@link approve}. {@link download} then returns without fetching it again.
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
   * {@link install} it takes effect the next time the app starts, or with
   * `requireApproval` at the first start after {@link approve}.
   *
   * Safe to call again for a bundle that is already stored: nothing is
   * fetched and only the `Finished` event is sent.
   *
   * `Finished` says the download is complete. The bundle is verified and
   * stored after it, so it is only ready once this promise resolves.
   *
   * Rejects with kind `inUse` if this device already stores other contents
   * under this version: a version names one bundle.
   */
  async download(
    onEvent?: (progress: DownloadEvent) => void,
    options?: DownloadOptions
  ): Promise<void> {
    await call('download', {
      version: this.version,
      onEvent: channel(onEvent),
      ...downloadArgs(options),
    })
    this.downloaded = true
  }

  /**
   * Switches to the downloaded bundle now by reloading the app's webviews.
   * In-memory page state is lost, as with any reload. The new frontend has to
   * call {@link notifyReady} or it is reverted.
   *
   * Does nothing when this version is already being served, so two windows
   * that install the same update reload the app once.
   *
   * A rejection of kind `tauri` means the switch happened but a window could
   * not be reloaded. With any other kind nothing was switched.
   */
  async install(): Promise<void> {
    await install(this.version)
  }

  /** {@link download} followed by {@link install}. */
  async downloadAndInstall(
    onEvent?: (progress: DownloadEvent) => void,
    options?: DownloadOptions
  ): Promise<void> {
    await call('download_and_install', {
      version: this.version,
      onEvent: channel(onEvent),
      ...downloadArgs(options),
    })
    this.downloaded = true
  }

  /**
   * Does nothing: unlike the updater plugin's `Update`, this one holds no
   * resource. It is here so that code written for both can call it.
   */
  async close(): Promise<void> {}
}

/** `HeadersInit` as the name and value pairs the commands take. */
function headerPairs(headers?: HeadersInit): [string, string][] | undefined {
  try {
    return headers ? Array.from(new Headers(headers).entries()) : undefined
  } catch (error) {
    // `Headers` throws a `TypeError` for a name or value it cannot send
    throw new OtaUpdateError('invalidHeader', String(error))
  }
}

function downloadArgs(options?: DownloadOptions) {
  return {
    headers: headerPairs(options?.headers),
    timeout: options?.timeout,
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
  const metadata = await call<UpdateMetadata | null>('check', {
    headers: headerPairs(options?.headers),
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
 * What is being served and whether it is confirmed, what is staged and what is
 * approved for the next launch, and which versions were rolled back. Makes no
 * request, so it works offline.
 */
export async function status(): Promise<Status> {
  return await call('status')
}

/**
 * Switches to the stored bundle `version` now by reloading the app's
 * webviews, like {@link Update.install}, but without a {@link check}: a bundle
 * that {@link status} reports as `staged` can be applied offline. Does nothing
 * when `version` is already being served.
 *
 * Rejects with kind `cannotServe` where no downloaded frontend can be shown:
 * under `tauri dev`, when the app did not call `wrap_assets`, or when its
 * `frontendDist` is a remote URL.
 */
export async function install(version: string): Promise<void> {
  await call('install', { version })
}

/**
 * Approves the stored bundle `version` for the next launch. Nothing changes in
 * this run of the app: use {@link install} to switch now.
 *
 * This is the consent an app configured with `requireApproval` waits for. There
 * a downloaded bundle stays stored, across restarts, until it is approved or
 * installed. Without `requireApproval` the next launch takes the newest stored
 * bundle anyway and this only records the choice.
 *
 * One version is approved at a time; approving another replaces it. Makes no
 * request, so it works offline.
 *
 * Rejects with kind `notDownloaded` for a version that is not stored,
 * `notNewer` for one older than the frontend being served and `rejected` for
 * one that was rolled back.
 */
export async function approve(version: string): Promise<void> {
  await call('approve', { version })
}

/**
 * Takes back the approval of `version`, for a user who changed their mind.
 * The bundle stays stored and can be approved again. Does nothing if that
 * version is not the approved one, and nothing about a bundle that was shown
 * before: that one is abandoned with {@link rollback}.
 */
export async function unapprove(version: string): Promise<void> {
  await call('unapprove', { version })
}

/**
 * Deletes the stored bundle `version` so that the next launch does not switch
 * to it, for an update the user declined. The version can be downloaded
 * again. Rejects with kind `inUse` for the bundle being served and for the
 * confirmed one kept to fall back on.
 *
 * A {@link Update.download} of that version that is still running is not
 * cancelled and stores the bundle again: wait for it first.
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
 * Rejects with kind `io` if the rejection could not be recorded. The app has
 * gone back for this run all the same, but the version is served or offered
 * again after a restart.
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
 * shows the previous frontend cannot vouch for the new one. Neither can a
 * webview the plugin saw no page load for.
 *
 * A downloaded bundle that was shown in two runs of the app (`trialAttempts`)
 * without confirming itself is reverted to the last confirmed one (or the
 * built-in frontend) and its version is never offered again. Disable with
 * `"rollback": false`.
 */
export async function notifyReady(): Promise<void> {
  await call('notify_ready')
}
