/**
 * Resolution stub for tests that mock the optional sibling Flash worker.
 * Importing this implementation directly is a test setup error.
 */
export class FlashWorkerClient {
	constructor() {
		throw new Error("FlashWorkerClient must be mocked in Vitest tests");
	}
}
