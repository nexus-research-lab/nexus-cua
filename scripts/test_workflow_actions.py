import tempfile
import unittest
from pathlib import Path

import check_workflow_actions


class WorkflowActionTests(unittest.TestCase):
    def write_workflow(self, root: Path, uses: list[str]) -> None:
        workflow = root / ".github/workflows/test.yml"
        workflow.parent.mkdir(parents=True)
        workflow.write_text(
            "name: test\nsteps:\n"
            + "".join(f"  - uses: {action}\n" for action in uses),
            encoding="utf-8",
        )

    def test_exact_commit_and_local_action_are_accepted(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.write_workflow(root, [f"actions/checkout@{'a' * 40}", "./local"])
            checked, failures = check_workflow_actions.validate(root)
            self.assertEqual(checked, 1)
            self.assertEqual(failures, [])

    def test_floating_or_noncanonical_refs_are_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.write_workflow(
                root,
                ["actions/checkout@v7", f"actions/setup-python@{'A' * 40}"],
            )
            checked, failures = check_workflow_actions.validate(root)
            self.assertEqual(checked, 2)
            self.assertEqual(len(failures), 2)


if __name__ == "__main__":
    unittest.main()
