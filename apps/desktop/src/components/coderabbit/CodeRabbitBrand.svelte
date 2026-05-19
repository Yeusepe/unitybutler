<script lang="ts">
	import orangeLogomark from "$lib/assets/branding/coderabbit-logomark.svg";
	import whiteLogomark from "$lib/assets/branding/coderabbit-logomark-white.svg";
	import orangeTypemark from "$lib/assets/branding/coderabbit-typemark.svg";
	import whiteTypemark from "$lib/assets/branding/coderabbit-typemark-white.svg";

	type Props = {
		showTypemark?: boolean;
		size?: "small" | "medium";
	};

	const { showTypemark = false, size = "small" }: Props = $props();
</script>

<span class="coderabbit-brand" class:medium={size === "medium"}>
	<span class="mark-wrap">
		<img src={orangeLogomark} alt="" class="mark mark--light" />
		<img src={whiteLogomark} alt="" class="mark mark--dark" />
	</span>
	{#if showTypemark}
		<span class="typemark-wrap">
			<img src={orangeTypemark} alt="CodeRabbit" class="typemark typemark--light" />
			<img src={whiteTypemark} alt="CodeRabbit" class="typemark typemark--dark" />
		</span>
	{/if}
</span>

<style lang="postcss">
	.coderabbit-brand {
		display: inline-flex;
		align-items: center;
		gap: 6px;
	}

	.mark-wrap,
	.typemark-wrap {
		display: inline-grid;
		align-items: center;
	}

	.mark,
	.typemark {
		display: block;
		grid-area: 1 / 1;
	}

	.mark {
		width: 13px;
		height: 13px;
	}

	.typemark {
		width: 104px;
		height: auto;
	}

	.medium {
		gap: 8px;

		.mark {
			width: 22px;
			height: 22px;
		}

		.typemark {
			width: 108px;
		}
	}

	.mark--dark,
	.typemark--dark {
		display: none;
	}

	:global(html.dark) {
		.mark--light,
		.typemark--light {
			display: none;
		}

		.mark--dark,
		.typemark--dark {
			display: block;
		}
	}
</style>
