<script lang="ts">
	import CodeRabbitBrand from "$components/coderabbit/CodeRabbitBrand.svelte";
	import { showError } from "$lib/error/showError";
	import { CODERABBIT_SERVICE } from "$lib/coderabbit/coderabbit";
	import { inject } from "@gitbutler/core/context";
	import { Button, CardGroup, Spacer } from "@gitbutler/ui";

	type Props = {
		projectId: string;
	};

	const { projectId }: Props = $props();

	const codeRabbitService = inject(CODERABBIT_SERVICE);
	const statusQuery = $derived(codeRabbitService.status(projectId));
	const status = $derived(statusQuery.response);
	const login = codeRabbitService.login;
	const writeDefaultConfig = codeRabbitService.writeDefaultConfig;

	let busy = $state(false);

	const statusLabel = $derived.by(() => {
		if (statusQuery.result.isLoading) return "Checking CodeRabbit CLI...";
		if (!status?.cliAvailable) return "CodeRabbit CLI not found";
		if (!status.authenticated) return "CodeRabbit CLI found, sign-in required";
		return `Signed in${status.username ? ` as ${status.username}` : ""}`;
	});

	async function signIn() {
		try {
			busy = true;
			await login({ projectId });
		} catch (error) {
			showError("CodeRabbit sign-in failed", error);
		} finally {
			busy = false;
		}
	}

	async function createConfig() {
		try {
			busy = true;
			await writeDefaultConfig({ projectId });
		} catch (error) {
			showError("Failed to create CodeRabbit config", error);
		} finally {
			busy = false;
		}
	}
</script>

<CardGroup.Item standalone>
	<div class="coderabbit-header">
		<CodeRabbitBrand showTypemark size="medium" />
	</div>
	<p class="text-12 text-body">
		CodeRabbit reviews run through the local CodeRabbit CLI and render recommendations directly in
		GitButler diffs.
	</p>
</CardGroup.Item>

<Spacer margin={10} dotted />

<CardGroup>
	<CardGroup.Item>
		{#snippet title()}
			CLI status
		{/snippet}
		{#snippet caption()}
			{statusLabel}
			{#if status?.version}
				<span> Version {status.version}.</span>
			{/if}
			{#if status?.error}
				<span> {status.error}</span>
			{/if}
		{/snippet}
		{#snippet actions()}
			<Button kind="outline" size="tag" icon="refresh" onclick={() => statusQuery.result.refetch()}>
				Check
			</Button>
		{/snippet}
	</CardGroup.Item>

	<CardGroup.Item>
		{#snippet title()}
			Account
		{/snippet}
		{#snippet caption()}
			{#if status?.authenticated}
				CodeRabbit is authenticated for this project.
			{:else}
				Sign in with the CodeRabbit CLI before running reviews.
			{/if}
		{/snippet}
		{#snippet actions()}
			<Button
				kind="outline"
				size="tag"
				icon="open-in-browser"
				disabled={busy || !status?.cliAvailable}
				onclick={signIn}
			>
				Sign in
			</Button>
		{/snippet}
	</CardGroup.Item>

	<CardGroup.Item>
		{#snippet title()}
			Project config
		{/snippet}
		{#snippet caption()}
			{#if status?.configExists}
				This project has a .coderabbit.yaml file. GitButler will not overwrite it automatically.
			{:else}
				Create a starter config that skips Unity raw files, dependency caches, generated output, and
				other noisy files.
			{/if}
		{/snippet}
		{#snippet actions()}
			<Button
				kind="outline"
				size="tag"
				icon="file-plus"
				disabled={busy || !status?.cliAvailable || status?.configExists}
				onclick={createConfig}
			>
				Create config
			</Button>
		{/snippet}
	</CardGroup.Item>
</CardGroup>

<style lang="postcss">
	.coderabbit-header {
		display: flex;
		align-items: center;
		gap: 8px;
	}
</style>
