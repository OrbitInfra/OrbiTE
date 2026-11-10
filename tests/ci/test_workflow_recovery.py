"""Execute the workflow's exact inline policy with a local, non-network API."""
import json
from pathlib import Path
import subprocess
import textwrap
import unittest

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = textwrap.dedent((ROOT / ".github/workflows/recover-empty-failure.yml").read_text().split("script: |\n", 1)[1])


class Recovery(unittest.TestCase):
    def invoke(self, run, jobs):
        program = '''
const [script, run, jobs] = JSON.parse(process.argv[1]);
let calls = 0;
const context = {repo: {owner: 'example', repo: 'test'}, payload: {workflow_run: {id: 10}}};
const github = {rest: {actions: {
  getWorkflowRun: async () => ({data: run}),
  listJobsForWorkflowRunAttempt: () => {},
  reRunWorkflow: async () => { calls++; },
}}, paginate: async () => jobs};
const core = {info: () => {}};
(async () => { await new (Object.getPrototypeOf(async function(){}).constructor)('context','github','core',script)(context,github,core); console.log(calls); })().catch(e => {console.error(e); process.exitCode=1;});
'''
        return int(subprocess.check_output(["node", "-e", program, json.dumps([SCRIPT, run, jobs])], text=True))

    def test_only_a_first_failure_without_unsuccessful_created_jobs_retries(self):
        run = dict(status="completed", conclusion="failure", run_attempt=1)
        for jobs in ([], [dict(status="completed", conclusion="success"), dict(status="completed", conclusion="skipped")]):
            self.assertEqual(self.invoke(run, jobs), 1)
        for conclusion in ("failure", "timed_out", "cancelled", "action_required", None):
            self.assertEqual(self.invoke(run, [dict(status="completed", conclusion=conclusion)]), 0)
        for changes in (dict(run_attempt=2), dict(status="in_progress"), dict(conclusion="success")):
            self.assertEqual(self.invoke({**run, **changes}, []), 0)
        self.assertEqual(self.invoke(run, [dict(status="in_progress", conclusion=None)]), 0)
        for event in ("push", "workflow_dispatch"):
            self.assertEqual(self.invoke({**run, "name": "Release preparation", "event": event}, []), 0)
        self.assertEqual(self.invoke({**run, "name": "Release preparation", "event": "pull_request"}, []), 1)


if __name__ == "__main__":
    unittest.main()
