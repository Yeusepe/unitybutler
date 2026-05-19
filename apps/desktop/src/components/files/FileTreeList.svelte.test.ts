import FileTreeList from "$components/files/FileTreeList.svelte";
import { fireEvent, render, screen } from "@testing-library/svelte";
import { describe, expect, test } from "vitest";
import type { TreeChange } from "@gitbutler/but-sdk";

function change(path: string): TreeChange {
	return {
		path,
		status: {
			type: "Modification",
			subject: {
				previousState: { id: "", kind: "Blob" },
				state: { id: "", kind: "Blob" },
				flags: null,
			},
		},
	} as TreeChange;
}

describe("FileTreeList", () => {
	test("collapses folder children when clicking the folder toggle", async () => {
		render(FileTreeList, {
			changes: [change("src/a.ts"), change("src/b.ts"), change("test/c.ts")],
			listMode: "tree",
		});

		expect(screen.getByText("a.ts")).toBeInTheDocument();
		expect(screen.getByText("b.ts")).toBeInTheDocument();

		await fireEvent.click(screen.getAllByLabelText("Toggle folder")[0]!);

		expect(screen.queryByText("a.ts")).not.toBeInTheDocument();
		expect(screen.queryByText("b.ts")).not.toBeInTheDocument();
		expect(screen.getByText("c.ts")).toBeInTheDocument();
	});
});
