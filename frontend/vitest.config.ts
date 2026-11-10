import path from "node:path";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vitest/config";

export default defineConfig({
	plugins: [react()],
	test: {
		environment: "jsdom",
		globals: true,
		setupFiles: ["./tests/setup.ts"],
		include: ["tests/**/*.test.{ts,tsx}"],
	},
	resolve: {
		alias: {
			"@": path.resolve(__dirname, "./"),
			// These unit tests mock the optional Flash worker client. Keep its
			// module resolvable without requiring the sibling oqto-flash checkout.
			"@oqto-flash/worker-client": path.resolve(
				__dirname,
				"tests/mocks/oqto-flash-worker-client.ts",
			),
			"@oqto-flash": path.resolve(__dirname, "../..", "oqto-flash/src"),
			"@byteowlz/design-system": path.resolve(
				__dirname,
				"vendor/design-system/src/index.ts",
			),
		},
	},
});
