import { getStackName, type Stack } from "$lib/stacks/stack";
import type {
	BaseBranchResolutionApproach,
	BaseBranchResolution,
	ResolutionApproach,
	StackStatus,
} from "@gitbutler/but-sdk";
import type { UnityConflictDocument } from "$lib/files/unityConflicts";

export type StackStatusInfoV3 = { stack: Stack; status: StackStatus };

export type StackStatusesWithBranchesV3 =
	| {
			type: "upToDate";
	  }
	| {
			type: "updatesRequired";
			worktreeConflicts: string[];
			subject: StackStatusInfoV3[];
	  };

export type WorktreeConflictPreview = {
	path: string;
	mode: "mergePreview" | "chooseSide";
	sessionId: string;
	lfs: UnityConflictLfsInfo;
	document?: UnityConflictDocument | null;
	availableChoices: UnityConflictSide[];
	message?: string | null;
};

export type UnityConflictSide = "local" | "upstream";

export type UnityConflictLfsInfo = {
	tracked: boolean;
	base: UnityConflictSideState;
	local: UnityConflictSideState;
	upstream: UnityConflictSideState;
};

export type UnityConflictSideState = {
	state: "textReady" | "missingLfsObject" | "binaryOrNonUtf8" | "pointerStillPresent" | "absent";
	size?: number | null;
};

export type UnityConflictResolutionInput = {
	sessionId: string;
	path: string;
	resolution:
		| {
				type: "blocks";
				blocks: Record<string, string>;
		  }
		| {
				type: UnityConflictSide;
		  };
};

export function stackFullyIntegrated(stackStatus: StackStatus): boolean {
	return (
		stackStatus.branchStatuses.every((branchStatus) => branchStatus.status.type === "integrated") &&
		stackStatus.treeStatus.type === "empty"
	);
}

export function getBaseBranchResolution(
	targetCommitOid: string | undefined,
	approach: BaseBranchResolutionApproach,
): BaseBranchResolution | undefined {
	if (!targetCommitOid) return;

	return {
		targetCommitOid,
		approach,
	};
}

export function sortStatusInfoV3(a: StackStatusInfoV3, b: StackStatusInfoV3): number {
	if (
		(!stackFullyIntegrated(a.status) && !stackFullyIntegrated(b.status)) ||
		(stackFullyIntegrated(a.status) && stackFullyIntegrated(b.status))
	) {
		const aName = (a.stack && getStackName(a.stack)) ?? "Unknown";
		const bName = (b.stack && getStackName(b.stack)) ?? "Unknown";

		return aName.localeCompare(bName);
	}

	if (stackFullyIntegrated(a.status)) {
		return 1;
	} else {
		return -1;
	}
}

export function getResolutionApproachV3(statusInfo: StackStatusInfoV3): ResolutionApproach {
	if (stackFullyIntegrated(statusInfo.status)) {
		return { type: "delete" };
	}

	return { type: "rebase" };
}
