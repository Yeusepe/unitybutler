<script lang="ts">
	import CodeRabbitBrand from "$components/coderabbit/CodeRabbitBrand.svelte";
	import { showError } from "$lib/error/showError";
	import { CODERABBIT_SERVICE } from "$lib/coderabbit/coderabbit";
	import { inject } from "@gitbutler/core/context";
	import { Button, chipToasts } from "@gitbutler/ui";
	import { onDestroy } from "svelte";
	import type { CodeRabbitWorkflowId } from "$lib/coderabbit/coderabbit";

	type Props = {
		projectId: string;
		files?: string[];
		base?: string;
		reviewType?: "all" | "committed" | "uncommitted";
		compact?: boolean;
	};

	const {
		projectId,
		files = [],
		base,
		reviewType = "uncommitted",
		compact = false,
	}: Props = $props();

	const codeRabbitService = inject(CODERABBIT_SERVICE);
	const statusQuery = $derived(codeRabbitService.status(projectId));
	const findingsQuery = $derived(codeRabbitService.findings(projectId));
	const status = $derived(statusQuery.response);
	const findings = $derived(
		(findingsQuery.response ?? []).filter((finding) => finding.status === "open"),
	);
	const review = codeRabbitService.review;
	const login = codeRabbitService.login;
	const cancel = codeRabbitService.cancel;
	const writeDefaultConfig = codeRabbitService.writeDefaultConfig;
	let workflowMenuOpen = $state(false);
	let activeWorkflow = $state<CodeRabbitWorkflowId>("default");
	let activeReviewId = $state<string | undefined>();
	let reviewing = $state(false);
	let cancelling = $state(false);
	let statusPopoverOpen = $state(false);
	let now = $state(Date.now());
	let interval: ReturnType<typeof setInterval> | undefined;

	const isReviewing = $derived(reviewing || !!activeReviewId || !!status?.activeReviewId);
	const activeProgress = $derived(status?.activeProgress);
	const lastReview = $derived(status?.lastReview);
	const reviewStartedAt = $derived(activeProgress?.startedAtMs ?? now);
	const elapsedLabel = $derived(formatDuration(now - reviewStartedAt));
	const lastUpdateLabel = $derived(
		activeProgress?.updatedAtMs ? formatDuration(now - activeProgress.updatedAtMs) : undefined,
	);
	const buttonLabel = $derived.by(() => {
		if (isReviewing)
			return compact
				? `CodeRabbit review ${elapsedLabel}`
				: `${activeProgress?.phase ?? "Reviewing"} ${elapsedLabel}`;
		if (!status?.cliAvailable) return "CodeRabbit unavailable";
		if (!status.authenticated) return "Sign in to CodeRabbit";
		if (findings.length > 0) return `CodeRabbit (${findings.length})`;
		if (lastReview?.status === "noFindings") return "No CodeRabbit findings";
		if (lastReview?.status === "complete") return `CodeRabbit (${lastReview.findingsCount})`;
		if (lastReview?.status === "failed") return "CodeRabbit failed";
		if (lastReview?.status === "cancelled") return "CodeRabbit cancelled";
		return compact ? "CodeRabbit" : "Review with CodeRabbit";
	});
	const buttonTooltip = $derived.by(() => {
		if (isReviewing) return undefined;
		if (lastReview) return lastReview.message;
		return status?.username ? `Signed in as ${status.username}` : status?.error;
	});
	const popoverTitle = $derived.by(() => {
		if (isReviewing) return activeProgress?.phase ?? "CodeRabbit review running";
		if (lastReview) return lastReview.message;
		if (!status?.cliAvailable) return "CodeRabbit unavailable";
		if (!status.authenticated) return "Sign in to CodeRabbit";
		return "CodeRabbit ready";
	});
	const popoverDetail = $derived.by(() => {
		if (isReviewing) return activeProgress?.detail ?? "Starting CodeRabbit review.";
		if (lastReview) return reviewSummaryDetail();
		if (status?.error) return status.error;
		if (status?.username) return `Signed in as ${status.username}.`;
		return "Run a review to see inline recommendations in the diff.";
	});

	$effect(() => {
		if (!isReviewing) {
			if (interval) clearInterval(interval);
			interval = undefined;
			return;
		}
		interval ??= setInterval(() => {
			now = Date.now();
			statusQuery.result.refetch();
			findingsQuery.result.refetch();
		}, 1000);
		return () => {
			if (interval) clearInterval(interval);
			interval = undefined;
		};
	});

	onDestroy(() => {
		if (interval) clearInterval(interval);
	});

	async function runReview(workflows: CodeRabbitWorkflowId[] = ["default"]) {
		activeWorkflow = workflows[0] ?? "default";
		if (!status?.cliAvailable) {
			showError("CodeRabbit CLI unavailable", status?.error ?? "Install the CodeRabbit CLI first.");
			return;
		}
		if (!status.authenticated) {
			await login({ projectId });
			return;
		}
		try {
			reviewing = true;
			cancelling = false;
			const reviewId = newReviewId();
			activeReviewId = reviewId;
			const result = await review({
				projectId,
				request: {
					reviewId,
					reviewType,
					base,
					files,
					workflows,
				},
			});
			if (result.findings.length === 0) {
				chipToasts.success("CodeRabbit completed with no recommendations");
			} else {
				chipToasts.success(`CodeRabbit found ${result.findings.length} recommendations`);
			}
		} catch (error) {
			if (!cancelling) {
				showError("CodeRabbit review failed", error);
			} else {
				chipToasts.warning("CodeRabbit review cancelled");
			}
		} finally {
			reviewing = false;
			cancelling = false;
			activeReviewId = undefined;
		}
	}

	async function cancelReview() {
		const reviewId = activeReviewId ?? status?.activeReviewId;
		if (!reviewId) return;
		try {
			cancelling = true;
			await cancel({ projectId, reviewId });
		} catch (error) {
			showError("Failed to cancel CodeRabbit review", error);
		}
	}

	function newReviewId() {
		return globalThis.crypto?.randomUUID?.() ?? `${Date.now()}-${Math.random()}`;
	}

	function formatDuration(ms: number) {
		const totalSeconds = Math.max(0, Math.floor(ms / 1000));
		const minutes = Math.floor(totalSeconds / 60);
		const seconds = totalSeconds % 60;
		if (minutes === 0) return `${seconds}s`;
		return `${minutes}m ${seconds.toString().padStart(2, "0")}s`;
	}

	function stepStatusLabel(status: "pending" | "running" | "complete" | "failed") {
		switch (status) {
			case "pending":
				return "Waiting";
			case "running":
				return "Running";
			case "complete":
				return "Done";
			case "failed":
				return "Failed";
		}
	}

	function reviewSummaryDetail() {
		if (!lastReview) return "";
		switch (lastReview.status) {
			case "complete":
				return `${lastReview.findingsCount} open recommendations are available in the diff.`;
			case "noFindings":
				return "CodeRabbit finished successfully and did not return recommendations.";
			case "failed":
				return "The review failed. Hover details are preserved until the next run.";
			case "cancelled":
				return "The review was cancelled before CodeRabbit returned results.";
		}
	}

	async function createConfig() {
		try {
			await writeDefaultConfig({ projectId });
			workflowMenuOpen = false;
		} catch (error) {
			showError("Failed to create CodeRabbit config", error);
		}
	}
</script>

<div class="coderabbit-review">
	<div
		role="presentation"
		class="review-button-wrap"
		onmouseenter={() => (statusPopoverOpen = true)}
		onmouseleave={() => (statusPopoverOpen = false)}
	>
		<Button
			type="button"
			kind="outline"
			size="tag"
			icon={isReviewing ? "spinner" : undefined}
			disabled={isReviewing || statusQuery.result.isLoading}
			tooltip={buttonTooltip}
			onclick={() => runReview(["default"])}
		>
			<span class="button-content">
				<CodeRabbitBrand />
				<span>{buttonLabel}</span>
			</span>
		</Button>

		{#if statusPopoverOpen && (isReviewing || lastReview || status?.error)}
			<div class="status-popover">
				<div class="status-popover__header">
					<CodeRabbitBrand />
					<div class="status-popover__title">
						<strong>{popoverTitle}</strong>
						<span>{popoverDetail}</span>
					</div>
				</div>

				<div class="status-meta">
					<span>Elapsed {elapsedLabel}</span>
					{#if isReviewing && lastUpdateLabel}
						<span>Last update {lastUpdateLabel} ago</span>
					{/if}
				</div>

				{#if activeProgress?.steps?.length}
					<div class="steps">
						{#each activeProgress.steps as step}
							<div class="step" data-status={step.status}>
								<span class="step-dot"></span>
								<div class="step-text">
									<div>
										<strong>{step.label}</strong>
										<span>{stepStatusLabel(step.status)}</span>
									</div>
									{#if step.detail}
										<p>{step.detail}</p>
									{/if}
								</div>
							</div>
						{/each}
					</div>
				{/if}
			</div>
		{/if}
	</div>
	<Button
		type="button"
		kind="ghost"
		size="tag"
		icon={isReviewing ? "cross" : "chevron-down"}
		tooltip={isReviewing ? "Cancel CodeRabbit review" : "CodeRabbit review workflows"}
		onclick={() => (isReviewing ? cancelReview() : (workflowMenuOpen = !workflowMenuOpen))}
	/>

	{#if workflowMenuOpen}
		<div class="workflow-menu">
			<button
				type="button"
				class:active={activeWorkflow === "default"}
				onclick={() => runReview(["default"])}
			>
				Default review
			</button>
			<button
				type="button"
				class:active={activeWorkflow === "performance"}
				onclick={() => runReview(["performance"])}
			>
				Performance review
			</button>
			<button
				type="button"
				class:active={activeWorkflow === "security"}
				onclick={() => runReview(["security"])}
			>
				Security review
			</button>
			<button
				type="button"
				class:active={activeWorkflow === "correctness"}
				onclick={() => runReview(["correctness"])}
			>
				Correctness review
			</button>
			{#if status?.cliAvailable && !status.configExists}
				<button type="button" onclick={createConfig}>Create .coderabbit.yaml</button>
			{/if}
		</div>
	{/if}
</div>

<style lang="postcss">
	.coderabbit-review {
		display: flex;
		position: relative;
		align-items: center;
		gap: 4px;
	}

	.review-button-wrap {
		position: relative;
	}

	.status-popover {
		display: flex;
		z-index: var(--z-popover);
		position: absolute;
		top: calc(100% + 6px);
		left: 0;
		flex-direction: column;
		width: 330px;
		padding: 10px;
		gap: 10px;
		border: 1px solid var(--border-2);
		border-radius: var(--radius-m);
		background-color: var(--bg-1);
		box-shadow: var(--fx-shadow-m);
		color: var(--text-1);
	}

	.status-popover__header {
		display: flex;
		align-items: flex-start;
		gap: 8px;
	}

	.status-popover__title {
		display: flex;
		flex-direction: column;
		min-width: 0;
		gap: 2px;

		strong {
			font-size: 12px;
			line-height: 1.25;
		}

		span {
			color: var(--text-2);
			font-size: 11px;
			line-height: 1.35;
		}
	}

	.status-meta {
		display: flex;
		flex-wrap: wrap;
		gap: 6px;

		span {
			padding: 2px 6px;
			border: 1px solid var(--border-2);
			border-radius: var(--radius-s);
			background-color: var(--bg-0);
			color: var(--text-2);
			font-size: 11px;
			line-height: 1.25;
		}
	}

	.steps {
		display: flex;
		flex-direction: column;
		gap: 7px;
	}

	.step {
		display: grid;
		grid-template-columns: 10px minmax(0, 1fr);
		align-items: flex-start;
		gap: 8px;
	}

	.step-dot {
		width: 8px;
		height: 8px;
		margin-top: 4px;
		border: 1px solid var(--border-2);
		border-radius: 999px;
		background-color: var(--bg-2);
	}

	.step[data-status="running"] .step-dot {
		border-color: var(--clr-theme-pop-element);
		background-color: var(--clr-theme-pop-element);
	}

	.step[data-status="complete"] .step-dot {
		border-color: var(--fill-safe-bg);
		background-color: var(--fill-safe-bg);
	}

	.step[data-status="failed"] .step-dot {
		border-color: var(--fill-danger-bg);
		background-color: var(--fill-danger-bg);
	}

	.step-text {
		display: flex;
		flex-direction: column;
		min-width: 0;
		gap: 2px;

		div {
			display: flex;
			align-items: baseline;
			justify-content: space-between;
			gap: 8px;
		}

		strong {
			font-size: 11px;
			line-height: 1.3;
		}

		span {
			flex-shrink: 0;
			color: var(--text-3);
			font-size: 10px;
			text-transform: uppercase;
		}

		p {
			margin: 0;
			color: var(--text-2);
			font-size: 11px;
			line-height: 1.35;
		}
	}

	.workflow-menu {
		display: flex;
		z-index: var(--z-popover);
		position: absolute;
		top: calc(100% + 6px);
		right: 0;
		flex-direction: column;
		width: 190px;
		padding: 4px;
		border: 1px solid var(--border-2);
		border-radius: var(--radius-m);
		background-color: var(--bg-1);
		box-shadow: var(--fx-shadow-m);

		button {
			padding: 7px 8px;
			border-radius: var(--radius-s);
			color: var(--text-1);
			font-size: 12px;
			text-align: left;

			&:hover,
			&.active {
				background-color: var(--bg-2);
			}
		}
	}

	.button-content {
		display: inline-flex;
		align-items: center;
		gap: 6px;
	}
</style>
