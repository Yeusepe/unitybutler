import IntegrateUpstreamModal from "$components/upstream/IntegrateUpstreamModal.svelte";
import { BACKEND } from "$lib/backend";
import { CLIPBOARD_SERVICE } from "$lib/backend/clipboard";
import { URL_SERVICE } from "$lib/backend/url";
import { BACKUP_SERVICE } from "$lib/backups/backupService.svelte";
import { BASE_BRANCH_SERVICE } from "$lib/baseBranch/baseBranchService.svelte";
import { FILE_SERVICE } from "$lib/files/fileService";
import { DEFAULT_FORGE_FACTORY } from "$lib/forge/forgeFactory.svelte";
import { STACK_SERVICE } from "$lib/stacks/stackService.svelte";
import { UPSTREAM_INTEGRATION_SERVICE } from "$lib/upstream/upstreamIntegrationService.svelte";
import { render, screen, waitFor } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, test, vi } from "vitest";

const injectMap = new Map<unknown, unknown>();

class ResizeObserverMock {
	observe() {}
	unobserve() {}
	disconnect() {}
}

vi.stubGlobal("ResizeObserver", ResizeObserverMock);

vi.mock("@gitbutler/core/context", () => ({
	InjectionToken: class {
		_key = Symbol();
	},
	inject(token: { _key: symbol }) {
		const value = injectMap.get(token);
		if (!value) {
			throw new Error("No mock for token");
		}
		return value;
	},
	injectOptional(token: { _key: symbol }) {
		return injectMap.get(token);
	},
}));

describe("IntegrateUpstreamModal", () => {
	beforeEach(() => {
		injectMap.clear();
		injectMap.set(FILE_SERVICE, {
			writeToWorkspace: vi.fn(),
		});
		injectMap.set(BACKUP_SERVICE, {
			getSettings: vi.fn().mockResolvedValue({
				backupDirectory: "",
				backupBeforeUpstreamDefault: true,
			}),
			updateSettings: vi.fn().mockResolvedValue(undefined),
			createBackup: vi.fn().mockResolvedValue(undefined),
		});
	});

	test("shows workspace update progress and transfer speed while integrating upstream", async () => {
		let progressHandler:
			| ((event: {
					payload: {
						direction: string;
						currentFile: number;
						totalFiles: number;
						fileDownloadedBytes: number;
						fileTotalBytes: number;
						progressPercent: number;
						bytesPerSecond?: number;
						path: string;
					};
			  }) => void)
			| undefined;
		let gitOperationProgressHandler:
			| ((event: {
					payload: {
						operation: string;
						phase: string;
						phaseLabel: string;
						elapsedMs: number;
						detail?: string;
					};
			  }) => void)
			| undefined;
		const listen = vi.fn((event: string, handler: typeof progressHandler) => {
			if (event.endsWith("/workspace_update_progress")) {
				progressHandler = handler;
			} else {
				gitOperationProgressHandler = handler as typeof gitOperationProgressHandler;
			}
			return async () => {};
		});

		let resolveIntegration: (() => void) | undefined;
		const integrateMutation = vi.fn(
			async () =>
				await new Promise<void>((resolve) => {
					resolveIntegration = resolve;
				}),
		);

		injectMap.set(BACKEND, { listen });
		injectMap.set(BASE_BRANCH_SERVICE, {
			baseBranch: () => ({ response: undefined }),
			refreshBaseBranch: vi.fn().mockResolvedValue(undefined),
		});
		injectMap.set(DEFAULT_FORGE_FACTORY, {
			current: {
				commitUrl: () => undefined,
			},
		});
		injectMap.set(UPSTREAM_INTEGRATION_SERVICE, {
			upstreamStatuses: vi.fn().mockResolvedValue({ type: "upToDate" }),
			integrateUpstream: () => [integrateMutation],
			resolveUpstreamIntegrationMutation: vi.fn(),
		});
		injectMap.set(STACK_SERVICE, {
			commitChanges: vi.fn(() => ({ response: undefined })),
		});
		injectMap.set(URL_SERVICE, { openExternalUrl: vi.fn() });
		injectMap.set(CLIPBOARD_SERVICE, { write: vi.fn() });
		const user = userEvent.setup();
		const { component } = render(IntegrateUpstreamModal, {
			props: {
				projectId: "project-1",
			},
		});

		await (component as { show: () => Promise<void> }).show();
		await user.click(await screen.findByRole("button", { name: "Update workspace" }));
		expect(await screen.findByRole("button", { name: "Updating workspace…" })).toBeInTheDocument();
		expect(screen.getByText("Preparing upstream integration. Elapsed 0s.")).toBeInTheDocument();

		await waitFor(() =>
			expect(listen).toHaveBeenCalledWith(
				"project://project-1/workspace_update_progress",
				expect.any(Function),
			),
		);
		await waitFor(() =>
			expect(listen).toHaveBeenCalledWith(
				"project://project-1/git_operation_progress",
				expect.any(Function),
			),
		);

		gitOperationProgressHandler?.({
			payload: {
				operation: "upstreamIntegration",
				phase: "treeMerge",
				phaseLabel: "Integrating upstream changes",
				elapsedMs: 4000,
				detail: "Git LFS hydration is deferred for this operation.",
			},
		});

		expect(await screen.findByText("Integrating upstream changes")).toBeInTheDocument();
		expect(
			screen.getByText("Git LFS hydration is deferred for this operation."),
		).toBeInTheDocument();

		progressHandler?.({
			payload: {
				direction: "download",
				currentFile: 3,
				totalFiles: 12,
				fileDownloadedBytes: 2 * 1024 * 1024,
				fileTotalBytes: 4 * 1024 * 1024,
				progressPercent: 60,
				bytesPerSecond: 12 * 1024 * 1024,
				path: "Assets/Bundles/world.bundle",
			},
		});

		expect(await screen.findByText("File 3 of 12")).toBeInTheDocument();
		expect(screen.getByText("60%")).toBeInTheDocument();
		expect(screen.getByText("12.0 MB/s")).toBeInTheDocument();
		expect(screen.getByText("Assets/Bundles/world.bundle")).toBeInTheDocument();
		expect(
			screen.getByText("Downloading file 3 of 12 at 12.0 MB/s. 60% complete."),
		).toBeInTheDocument();

		resolveIntegration?.();
	});

	test("shows progress while loading upstream status before update options are ready", async () => {
		const listen = vi.fn(() => async () => {});
		let resolveStatuses: (() => void) | undefined;
		const upstreamStatuses = vi.fn(
			(_projectId: string, _targetCommitOid: string | undefined, onProgress?: Function) =>
				new Promise((resolve) => {
					onProgress?.({
						phase: "status",
						phaseLabel: "Checking upstream status",
						detail: "Computing update options for the selected target commit.",
					});
					resolveStatuses = () => resolve({ type: "upToDate" });
				}),
		);

		injectMap.set(BACKEND, { listen });
		injectMap.set(BASE_BRANCH_SERVICE, {
			baseBranch: () => ({ response: undefined }),
			refreshBaseBranch: vi.fn().mockResolvedValue(undefined),
		});
		injectMap.set(DEFAULT_FORGE_FACTORY, {
			current: {
				commitUrl: () => undefined,
			},
		});
		injectMap.set(UPSTREAM_INTEGRATION_SERVICE, {
			upstreamStatuses,
			integrateUpstream: () => [vi.fn()],
			resolveUpstreamIntegrationMutation: vi.fn(),
		});
		injectMap.set(STACK_SERVICE, {
			commitChanges: vi.fn(() => ({ response: undefined })),
		});
		injectMap.set(URL_SERVICE, { openExternalUrl: vi.fn() });
		injectMap.set(CLIPBOARD_SERVICE, { write: vi.fn() });

		const { component } = render(IntegrateUpstreamModal, {
			props: {
				projectId: "project-1",
			},
		});

		const showPromise = (component as { show: () => Promise<void> }).show();

		expect(await screen.findByText("Checking upstream status")).toBeInTheDocument();
		expect(
			screen.getByText("Computing update options for the selected target commit."),
		).toBeInTheDocument();

		resolveStatuses?.();
		await showPromise;
	});

	test("shows predicted scene conflicts as clickable previews", async () => {
		const listen = vi.fn(() => async () => {});
		const integrateMutation = vi.fn().mockResolvedValue({ deletedBranches: [] });

		injectMap.set(BACKEND, { listen });
		injectMap.set(BASE_BRANCH_SERVICE, {
			baseBranch: () => ({ response: undefined }),
			refreshBaseBranch: vi.fn().mockResolvedValue(undefined),
		});
		injectMap.set(DEFAULT_FORGE_FACTORY, {
			current: {
				commitUrl: () => undefined,
			},
		});
		injectMap.set(UPSTREAM_INTEGRATION_SERVICE, {
			upstreamStatuses: vi.fn().mockResolvedValue({
				type: "updatesRequired",
				worktreeConflicts: ["Assets/Scenes/dealers.unity"],
				subject: [],
			}),
			integrateUpstream: () => [integrateMutation],
			resolveUpstreamIntegrationMutation: vi.fn(),
			worktreeConflictPreview: vi.fn(),
		});
		injectMap.set(STACK_SERVICE, {
			commitChanges: vi.fn(() => ({ response: undefined })),
		});
		injectMap.set(URL_SERVICE, { openExternalUrl: vi.fn() });
		injectMap.set(CLIPBOARD_SERVICE, { write: vi.fn() });

		const user = userEvent.setup();
		const { component } = render(IntegrateUpstreamModal, {
			props: {
				projectId: "project-1",
			},
		});

		await (component as { show: () => Promise<void> }).show();

		expect(
			await screen.findByRole("button", { name: /Assets\/Scenes\/dealers\.unity/i }),
		).toBeInTheDocument();
		expect(screen.getByText("Click to resolve")).toBeInTheDocument();
		expect(screen.queryByText("scene resolver")).not.toBeInTheDocument();
	});

	test("stores backend scene conflict sessions instead of writing full scene contents", async () => {
		const listen = vi.fn(() => async () => {});
		const integrateMutation = vi.fn().mockResolvedValue({ deletedBranches: [] });
		const applyUnityConflictResolution = vi.fn().mockResolvedValue(undefined);

		injectMap.set(BACKEND, { listen });
		injectMap.set(BASE_BRANCH_SERVICE, {
			baseBranch: () => ({ response: undefined }),
			refreshBaseBranch: vi.fn().mockResolvedValue(undefined),
		});
		injectMap.set(DEFAULT_FORGE_FACTORY, {
			current: {
				commitUrl: () => undefined,
			},
		});
		injectMap.set(UPSTREAM_INTEGRATION_SERVICE, {
			upstreamStatuses: vi.fn().mockResolvedValue({
				type: "updatesRequired",
				worktreeConflicts: ["Assets/Scenes/dealers.unity"],
				subject: [],
			}),
			integrateUpstream: () => [integrateMutation],
			resolveUpstreamIntegrationMutation: vi.fn(),
			applyUnityConflictResolution,
			worktreeConflictPreview: vi.fn().mockResolvedValue({
				path: "Assets/Scenes/dealers.unity",
				mode: "mergePreview",
				sessionId: "session-1",
				lfs: {
					tracked: true,
					base: { state: "textReady" },
					local: { state: "textReady" },
					upstream: { state: "textReady" },
				},
				availableChoices: ["local", "upstream"],
				document: {
					path: "Assets/Scenes/dealers.unity",
					blocks: [
						{
							id: "conflict-1",
							label: "m_Name",
							context: "GameObject",
							ours: "  m_Name: Local\n",
							theirs: "  m_Name: Upstream\n",
							fields: [],
						},
					],
				},
			}),
		});
		injectMap.set(STACK_SERVICE, {
			commitChanges: vi.fn(() => ({ response: undefined })),
		});
		injectMap.set(URL_SERVICE, { openExternalUrl: vi.fn() });
		injectMap.set(CLIPBOARD_SERVICE, { write: vi.fn() });

		const user = userEvent.setup();
		const { component } = render(IntegrateUpstreamModal, {
			props: {
				projectId: "project-1",
			},
		});

		await (component as { show: () => Promise<void> }).show();
		await user.click(
			await screen.findByRole("button", { name: /Assets\/Scenes\/dealers\.unity/i }),
		);
		await user.click(await screen.findByRole("button", { name: "Use ours for conflict 1" }));
		await user.click(screen.getByRole("button", { name: "Apply to scene" }));
		expect(await screen.findByText("Resolution ready")).toBeInTheDocument();

		await user.click(screen.getByRole("button", { name: "Update workspace" }));

		await waitFor(() =>
			expect(applyUnityConflictResolution).toHaveBeenCalledWith("project-1", {
				sessionId: "session-1",
				path: "Assets/Scenes/dealers.unity",
				resolution: {
					type: "blocks",
					blocks: {
						"conflict-1": "  m_Name: Local\n",
					},
				},
			}),
		);
		expect(injectMap.get(FILE_SERVICE)).toEqual({ writeToWorkspace: expect.any(Function) });
	});

	test("allows explicit side choice when scene merge preview falls back", async () => {
		const listen = vi.fn(() => async () => {});

		injectMap.set(BACKEND, { listen });
		injectMap.set(BASE_BRANCH_SERVICE, {
			baseBranch: () => ({ response: undefined }),
			refreshBaseBranch: vi.fn().mockResolvedValue(undefined),
		});
		injectMap.set(DEFAULT_FORGE_FACTORY, {
			current: {
				commitUrl: () => undefined,
			},
		});
		injectMap.set(UPSTREAM_INTEGRATION_SERVICE, {
			upstreamStatuses: vi.fn().mockResolvedValue({
				type: "updatesRequired",
				worktreeConflicts: ["Assets/Scenes/dealers.unity"],
				subject: [],
			}),
			integrateUpstream: () => [vi.fn()],
			resolveUpstreamIntegrationMutation: vi.fn(),
			applyUnityConflictResolution: vi.fn(),
			worktreeConflictPreview: vi.fn().mockResolvedValue({
				path: "Assets/Scenes/dealers.unity",
				mode: "chooseSide",
				sessionId: "session-2",
				lfs: {
					tracked: true,
					base: { state: "missingLfsObject", size: 500_000_000 },
					local: { state: "textReady" },
					upstream: { state: "textReady", size: 500_000_000 },
				},
				availableChoices: ["local", "upstream"],
				message: "GitButler could not build a pointer-safe scene merge preview.",
			}),
		});
		injectMap.set(STACK_SERVICE, {
			commitChanges: vi.fn(() => ({ response: undefined })),
		});
		injectMap.set(URL_SERVICE, { openExternalUrl: vi.fn() });
		injectMap.set(CLIPBOARD_SERVICE, { write: vi.fn() });

		const user = userEvent.setup();
		const { component } = render(IntegrateUpstreamModal, {
			props: {
				projectId: "project-1",
			},
		});

		await (component as { show: () => Promise<void> }).show();
		await user.click(
			await screen.findByRole("button", { name: /Assets\/Scenes\/dealers\.unity/i }),
		);
		expect(
			await screen.findByText("GitButler could not build a pointer-safe scene merge preview."),
		).toBeInTheDocument();
		await user.click(screen.getByRole("button", { name: "Use upstream" }));

		expect(await screen.findByText("Resolution ready")).toBeInTheDocument();
	});

	test("expands incoming commits to show touched files", async () => {
		const listen = vi.fn(() => async () => {});
		const commitChanges = vi.fn(() => ({
			response: {
				changes: [
					{
						path: "src/App.svelte",
						status: { type: "Modification" },
					},
					{
						path: "src/routes/new-route.ts",
						status: { type: "Addition" },
					},
				],
				stats: {
					linesAdded: 12,
					linesRemoved: 4,
					filesChanged: 2,
				},
			},
		}));

		injectMap.set(BACKEND, { listen });
		injectMap.set(BASE_BRANCH_SERVICE, {
			baseBranch: () => ({
				response: {
					upstreamCommits: [
						{
							id: "abcdef123456",
							description: "Add route details\n\nMore context",
							createdAt: Date.now(),
							author: { name: "Ivay" },
						},
					],
				},
			}),
			refreshBaseBranch: vi.fn().mockResolvedValue(undefined),
		});
		injectMap.set(DEFAULT_FORGE_FACTORY, {
			current: {
				commitUrl: () => undefined,
			},
		});
		injectMap.set(UPSTREAM_INTEGRATION_SERVICE, {
			upstreamStatuses: vi.fn().mockResolvedValue({ type: "upToDate" }),
			integrateUpstream: () => [vi.fn()],
			resolveUpstreamIntegrationMutation: vi.fn(),
		});
		injectMap.set(STACK_SERVICE, { commitChanges });
		injectMap.set(URL_SERVICE, { openExternalUrl: vi.fn() });
		injectMap.set(CLIPBOARD_SERVICE, { write: vi.fn() });

		const user = userEvent.setup();
		const { component } = render(IntegrateUpstreamModal, {
			props: {
				projectId: "project-1",
			},
		});

		await (component as { show: () => Promise<void> }).show();
		await user.click(await screen.findByRole("button", { name: /Add route details/ }));

		expect(commitChanges).toHaveBeenCalledWith("project-1", "abcdef123456");
		expect(screen.getByText("2 touched files")).toBeInTheDocument();
		expect(screen.getByText("src/App.svelte")).toBeInTheDocument();
		expect(screen.getByText("src/routes/new-route.ts")).toBeInTheDocument();
	});
});
