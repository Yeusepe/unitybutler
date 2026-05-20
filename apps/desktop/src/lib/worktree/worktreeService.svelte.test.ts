import {
	normalizeLocalIgnorePath,
	pathIsLocallyIgnored,
	WorktreeService,
} from "$lib/worktree/worktreeService.svelte";
import { describe, expect, test, vi } from "vitest";

describe("WorktreeService", () => {
	test("normalizes local ignore paths for matching", () => {
		expect(normalizeLocalIgnorePath(String.raw`Assets\Generated\NavMesh.asset`)).toBe(
			"Assets/Generated/NavMesh.asset",
		);
		expect(normalizeLocalIgnorePath("./Assets//Generated")).toBe("Assets/Generated");
		expect(normalizeLocalIgnorePath("../outside")).toBeUndefined();
	});

	test("matches locally ignored paths and their children", () => {
		const ignoredPaths = ["Assets/Generated", "ProjectSettings/EditorSettings.asset"];

		expect(pathIsLocallyIgnored("Assets/Generated/NavMesh.asset", ignoredPaths)).toBe(true);
		expect(pathIsLocallyIgnored("ProjectSettings/EditorSettings.asset", ignoredPaths)).toBe(true);
		expect(pathIsLocallyIgnored("Assets/Generated2/NavMesh.asset", ignoredPaths)).toBe(false);
	});

	test("setLocalIgnoredPath uses the mutation endpoint", async () => {
		const mutate = vi.fn().mockResolvedValue(undefined);
		const service = new WorktreeService({
			endpoints: {
				setLocalIgnoredPath: {
					mutate,
				},
			},
		} as never);

		await service.setLocalIgnoredPath("project-1", "Assets/Generated/NavMesh.asset", true);

		expect(mutate).toHaveBeenCalledWith({
			projectId: "project-1",
			path: "Assets/Generated/NavMesh.asset",
			ignored: true,
		});
	});

	test("clears the worktree filter before refreshing after unignore", async () => {
		let localIgnoredResponse = ["Assets/Generated"];
		const calls: string[] = [];
		let transformLocalIgnoredPaths: ((paths: string[]) => string[]) | undefined;
		const worktreeData = {
			rawChanges: [{ path: "Assets/Generated/NavMesh.asset" }],
			ignoredChanges: [],
			hunkAssignments: [],
			changes: { ids: [], entities: {} },
		};
		let transformWorktreeData: ((data: typeof worktreeData) => {
			rawChanges: { path: string }[];
		}) | undefined;

		const service = new WorktreeService({
			endpoints: {
				setLocalIgnoredPath: {
					mutate: vi.fn().mockResolvedValue(undefined),
				},
				worktreeChanges: {
					fetch: vi.fn().mockImplementation(async () => {
						calls.push("worktreeChanges.fetch");
						expect(transformWorktreeData?.(worktreeData).rawChanges).toEqual([
							{ path: "Assets/Generated/NavMesh.asset" },
						]);
					}),
					useQuery: vi.fn((_args, options) => {
						transformWorktreeData = options.transform;
					}),
				},
				localIgnoredPaths: {
					fetch: vi.fn().mockImplementation(async () => {
						calls.push("localIgnoredPaths.fetch");
					}),
					useQuery: vi.fn((_args, options) => {
						transformLocalIgnoredPaths = options.transform;
						return {
							get response() {
								return transformLocalIgnoredPaths?.(localIgnoredResponse);
							},
						};
					}),
				},
			},
		} as never);

		const localIgnoredPathsQuery = service.localIgnoredPaths("project-1");
		service.worktreeData("project-1");

		await service.setLocalIgnoredPath("project-1", "Assets/Generated", true);
		expect(localIgnoredPathsQuery.response).toEqual(["Assets/Generated"]);
		expect(transformWorktreeData?.(worktreeData).rawChanges).toEqual([]);

		localIgnoredResponse = [];
		await service.setLocalIgnoredPath("project-1", "Assets/Generated", false);

		expect(calls).toEqual(["worktreeChanges.fetch", "localIgnoredPaths.fetch"]);
		expect(localIgnoredPathsQuery.response).toEqual([]);
		expect(service["backendApi"].endpoints.worktreeChanges.fetch).toHaveBeenCalledWith(
			{ projectId: "project-1" },
			{ forceRefetch: true },
		);
	});

	test("serializes local ignore mutations per project", async () => {
		let releaseFirstMutation: (() => void) | undefined;
		const firstMutation = new Promise<void>((resolve) => {
			releaseFirstMutation = resolve;
		});
		const mutate = vi.fn().mockImplementationOnce(() => firstMutation).mockResolvedValue(undefined);
		const service = new WorktreeService({
			endpoints: {
				setLocalIgnoredPath: {
					mutate,
				},
			},
		} as never);

		const first = service.setLocalIgnoredPath("project-1", "Assets/Generated", true);
		const second = service.setLocalIgnoredPath("project-1", "ProjectSettings", true);

		await Promise.resolve();
		expect(mutate).toHaveBeenCalledTimes(1);

		releaseFirstMutation?.();
		await Promise.all([first, second]);

		expect(mutate).toHaveBeenCalledTimes(2);
		expect(mutate).toHaveBeenNthCalledWith(2, {
			projectId: "project-1",
			path: "ProjectSettings",
			ignored: true,
		});
	});
});
