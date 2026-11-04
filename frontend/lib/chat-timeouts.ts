interface OqtoRuntimeConfig {
	chatFirstResponseTimeoutMs?: number;
	chatStreamIdleTimeoutMs?: number;
}

declare global {
	interface Window {
		__OQTO_RUNTIME_CONFIG__?: OqtoRuntimeConfig;
	}
}

const MAX_TIMEOUT_MS = 2_147_483_647;

function readTimeout(value: number | undefined, fallback: number): number {
	if (
		typeof value !== "number" ||
		!Number.isSafeInteger(value) ||
		value <= 0 ||
		value > MAX_TIMEOUT_MS
	) {
		return fallback;
	}
	return value;
}

const runtimeConfig =
	typeof window === "undefined" ? undefined : window.__OQTO_RUNTIME_CONFIG__;

export const CHAT_FIRST_RESPONSE_TIMEOUT_MS = readTimeout(
	runtimeConfig?.chatFirstResponseTimeoutMs,
	180_000,
);

export const CHAT_STREAM_IDLE_TIMEOUT_MS = readTimeout(
	runtimeConfig?.chatStreamIdleTimeoutMs,
	60_000,
);