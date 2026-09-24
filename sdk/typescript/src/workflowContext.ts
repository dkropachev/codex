/** A selectable answer rendered by a hosted Codex workflow client. */
export type WorkflowUserInputOption = {
  /** Labels cannot begin with `user_note: `, which is reserved for typed text. */
  label: string;
  description: string;
};

/** A single-choice or free-text question requested by a hosted workflow. */
export type WorkflowUserInputQuestion = {
  id: string;
  header: string;
  question: string;
  /** Adds a synthetic `None of the above` option; that label is reserved. */
  isOther?: boolean;
  /** Masks supporting client UI; the plaintext answer is still returned to workflow code. */
  isSecret?: boolean;
  /** Omit for free-text input; provide labels for a single-choice question. */
  options?: WorkflowUserInputOption[] | null;
};

export type WorkflowUserInputRequest = {
  questions: WorkflowUserInputQuestion[];
};

export type WorkflowUserInputAnswer = {
  /** Selected label first, followed by an optional `user_note: ...` entry. */
  answers: string[];
};

export type WorkflowUserInputResponse = {
  answers: Record<string, WorkflowUserInputAnswer>;
};

/** Host capabilities supplied to `run()` by an interactive workflow command. */
export type WorkflowContext = {
  requestUserInput(request: WorkflowUserInputRequest): Promise<WorkflowUserInputResponse>;
  progress(message: string, data?: unknown): void;
  workingDirectory?: string;
  cwd?: string;
  currentWorkingDirectory?: string;
  repoRoot?: string;
};
