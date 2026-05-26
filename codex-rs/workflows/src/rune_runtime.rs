use std::env;
use std::future::Future;
use std::io::BufRead;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Stdio;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use anyhow::Context as _;
use anyhow::Result;
use anyhow::anyhow;
use rune::Any;
use rune::Diagnostics;
use rune::Module;
use rune::Source;
use rune::Sources;
use rune::Vm;
use rune::runtime::Function;
use rune::runtime::Object;
use rune::runtime::Protocol;
use rune::runtime::Ref;
use rune::runtime::Value;
use rune::runtime::VmResult;
use serde_json::Value as JsonValue;
use serde_json::json;

use crate::command::WorkflowCommandInput;
use crate::command_completion::WorkflowCommandCompletionSuggestion;
use crate::rune_agent::WorkflowRuneAgentHandle;
use crate::rune_api::WorkflowRuneApiNamespace;
use crate::rune_api::WorkflowRuneArtifactsNamespace;
use crate::rune_api::WorkflowRuneFsNamespace;
use crate::rune_api::WorkflowRuneMcpNamespace;
use crate::rune_api::WorkflowRuneProcessNamespace;
use crate::rune_api::WorkflowRuneToolsNamespace;
use crate::rune_api::WorkflowRuneWorkflowsNamespace;
use crate::rune_app_server;
use crate::rune_app_server::WorkflowRuneAppServer;
use crate::rune_input::WorkflowRuneInputHelpers;
use crate::rune_tool::WorkflowRuneDynamicTool;
use crate::workflow_runtime::WORKFLOW_NAME_ENV;
use crate::workflow_runtime::WORKFLOW_OUTPUT_FORMAT_ENV;
use crate::workflow_runtime::WORKFLOW_RUNTIME_EVENT_PREFIX;
use crate::workflow_runtime::WORKFLOW_SELF_EXE_ENV;
use crate::workflow_runtime::WORKFLOW_WORKING_DIRECTORY_ENV;
use crate::workflow_runtime::WorkflowChildStatus;
use crate::workflow_runtime::WorkflowRuntimeEvent;
use crate::workflow_runtime::WorkflowRuntimeOutput;
use crate::workflow_runtime::WorkflowStatusUpdate;
use crate::workflow_runtime::WorkflowThreadStatus;

pub(crate) fn validate_workflow_source(workflow_path: &Path) -> Result<()> {
    CompiledRuneWorkflow::compile(workflow_path).map(|_| ())
}

pub(crate) fn run_workflow_for_validation(
    working_directory: &Path,
    workflow_dir: &Path,
    workflow_path: &Path,
    input: &str,
) -> Result<WorkflowRuntimeOutput> {
    let working_directory = working_directory.to_path_buf();
    let workflow_dir = workflow_dir.to_path_buf();
    let workflow_path = workflow_path.to_path_buf();
    let input = input.to_string();
    std::thread::spawn(move || {
        run_rune_on_current_thread(async {
            run_workflow_inner(&working_directory, &workflow_dir, &workflow_path, &input).await
        })
    })
    .join()
    .map_err(|_| anyhow!("Rune workflow validation task panicked"))?
}

pub(crate) async fn run_workflow(
    working_directory: &Path,
    workflow_dir: &Path,
    workflow_path: &Path,
    input: &str,
) -> Result<WorkflowRuntimeOutput> {
    let working_directory = working_directory.to_path_buf();
    let workflow_dir = workflow_dir.to_path_buf();
    let workflow_path = workflow_path.to_path_buf();
    let input = input.to_string();
    tokio::task::spawn_blocking(move || {
        run_rune_on_current_thread(async {
            run_workflow_inner(&working_directory, &workflow_dir, &workflow_path, &input).await
        })
    })
    .await
    .context("Rune workflow task failed")?
}

async fn run_workflow_inner(
    working_directory: &Path,
    workflow_dir: &Path,
    workflow_path: &Path,
    input: &str,
) -> Result<WorkflowRuntimeOutput> {
    let runtime = match CompiledRuneWorkflow::compile(workflow_path) {
        Ok(runtime) => runtime,
        Err(err) => return Ok(runtime_error_output(err)),
    };
    let ctx = WorkflowRuneContext::new(workflow_dir, working_directory);
    let input = match serde_json::from_str::<Value>(input) {
        Ok(input) => input,
        Err(err) => {
            return Ok(runtime_error_output(anyhow!(
                "workflow input was not JSON: {err}"
            )));
        }
    };

    let result = match runtime.call_async("run", (ctx, input)).await {
        Ok(result) => result,
        Err(err) => return Ok(runtime_error_output(err)),
    };

    if let Err(err) = runtime.emit_requested_format(result.clone()).await {
        return Ok(runtime_error_output(err));
    }

    let stdout = match serde_json::to_string_pretty(&result) {
        Ok(stdout) => format!("{stdout}\n"),
        Err(err) => {
            return Ok(runtime_error_output(anyhow!(
                "workflow result was not JSON: {err}"
            )));
        }
    };
    Ok(WorkflowRuntimeOutput {
        stdout,
        stderr: String::new(),
        success: true,
        exit_status: "success".to_string(),
    })
}

pub(crate) async fn complete_workflow(
    workflow_dir: &Path,
    working_directory: &Path,
    workflow_path: &Path,
    input: &WorkflowCommandInput,
) -> Result<Vec<WorkflowCommandCompletionSuggestion>> {
    let workflow_dir = workflow_dir.to_path_buf();
    let working_directory = working_directory.to_path_buf();
    let workflow_path = workflow_path.to_path_buf();
    let input = input.clone();
    tokio::task::spawn_blocking(move || {
        run_rune_on_current_thread(async {
            complete_workflow_inner(&workflow_dir, &working_directory, &workflow_path, &input).await
        })
    })
    .await
    .context("Rune workflow completion task failed")?
}

async fn complete_workflow_inner(
    workflow_dir: &Path,
    working_directory: &Path,
    workflow_path: &Path,
    input: &WorkflowCommandInput,
) -> Result<Vec<WorkflowCommandCompletionSuggestion>> {
    let runtime = CompiledRuneWorkflow::compile(workflow_path)?;
    let ctx = WorkflowRuneContext::new(workflow_dir, working_directory);
    let input = serde_json::to_value(input)
        .and_then(serde_json::from_value::<Value>)
        .context("failed to convert workflow completion input for Rune")?;

    let result = match runtime
        .call_optional_async("complete", (ctx, input))
        .await?
    {
        Some(result) => result,
        None => return Ok(Vec::new()),
    };
    normalize_completion_suggestions(result)
}

fn run_rune_on_current_thread<T>(future: impl Future<Output = Result<T>>) -> Result<T> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to create Rune runtime task executor")?;
    let local = tokio::task::LocalSet::new();
    local.block_on(&runtime, future)
}

struct CompiledRuneWorkflow {
    runtime: Arc<rune::runtime::RuntimeContext>,
    unit: Arc<rune::runtime::Unit>,
}

impl CompiledRuneWorkflow {
    fn compile(workflow_path: &Path) -> Result<Self> {
        let mut context = rune_modules::with_config(true)?;
        context.install(codex_module()?)?;
        let runtime = Arc::new(context.runtime()?);
        let mut sources = Sources::new();
        sources.insert(Source::from_path(workflow_path).with_context(|| {
            format!(
                "failed to read Rune workflow source {}",
                workflow_path.display()
            )
        })?)?;
        let mut diagnostics = Diagnostics::default();
        let result = rune::prepare(&mut sources)
            .with_context(&context)
            .with_diagnostics(&mut diagnostics)
            .build();

        if !diagnostics.is_empty() {
            let mut out = rune::termcolor::Buffer::no_color();
            diagnostics.emit(&mut out, &sources)?;
            let detail = String::from_utf8_lossy(out.as_slice()).trim().to_string();
            if !detail.is_empty() {
                anyhow::bail!("{detail}");
            }
        }

        let unit = Arc::new(result?);
        Ok(Self { runtime, unit })
    }

    async fn call_async<A>(&self, name: &str, args: A) -> Result<Value>
    where
        A: rune::runtime::Args,
    {
        let mut vm = Vm::new(Arc::clone(&self.runtime), Arc::clone(&self.unit));
        let mut execution = vm.execute([name], args)?;
        execution
            .async_complete()
            .await
            .into_result()
            .map_err(Into::into)
    }

    async fn call_optional_async<A>(&self, name: &str, args: A) -> Result<Option<Value>>
    where
        A: rune::runtime::Args,
    {
        match self.call_async(name, args).await {
            Ok(value) => Ok(Some(value)),
            Err(err) if is_missing_rune_function(&err) => Ok(None),
            Err(err) => Err(err),
        }
    }

    async fn emit_requested_format(&self, result: Value) -> Result<()> {
        let requested_format = env::var(WORKFLOW_OUTPUT_FORMAT_ENV).unwrap_or_default();
        if requested_format.trim().is_empty() {
            return Ok(());
        }
        if requested_format != "tui.markdown.v1" {
            anyhow::bail!("unsupported host output format {requested_format}");
        }
        let Some(formatted) = self
            .call_optional_async("to_tui_markdown", (result,))
            .await?
        else {
            return Ok(());
        };
        let formatted = rune_value_to_json(formatted)?;
        let markdown = formatted
            .get("markdown")
            .and_then(JsonValue::as_str)
            .filter(|markdown| !markdown.trim().is_empty())
            .ok_or_else(|| {
                anyhow!("workflow format {requested_format} must return {{ markdown: string }}")
            })?;
        emit_report_to_user_markdown(markdown);
        Ok(())
    }
}

#[derive(Clone, Any)]
#[rune(item = ::codex)]
struct WorkflowRuneContext {
    inner: Rc<WorkflowRuneContextInner>,
}

struct WorkflowRuneContextInner {
    workflow_name: String,
    working_directory: String,
    self_exe: String,
    app_server: WorkflowRuneAppServer,
}

impl WorkflowRuneContext {
    fn new(workflow_dir: &Path, working_directory: &Path) -> Self {
        let workflow_name = env::var(WORKFLOW_NAME_ENV).unwrap_or_else(|_| {
            workflow_dir
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("workflow")
                .to_string()
        });
        let working_directory = env::var(WORKFLOW_WORKING_DIRECTORY_ENV)
            .unwrap_or_else(|_| working_directory.display().to_string());
        let self_exe = env::var_os(WORKFLOW_SELF_EXE_ENV)
            .map(PathBuf::from)
            .or_else(|| env::current_exe().ok())
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let app_server = WorkflowRuneAppServer::new(self_exe.clone(), working_directory.clone());
        Self {
            inner: Rc::new(WorkflowRuneContextInner {
                workflow_name,
                working_directory,
                self_exe,
                app_server,
            }),
        }
    }

    fn status(&self, status: Value) -> VmResult<()> {
        vm_result_from_result((|| {
            let status = normalize_status_update(rune_value_to_json(status)?, "status")?;
            emit_status(&status);
            Ok(())
        })())
    }

    fn progress(&self, message: &str, data: Value) -> VmResult<()> {
        vm_result_from_result((|| {
            let data = rune_value_to_json(data)?;
            let status = legacy_progress_to_status(
                &self.inner.workflow_name,
                message,
                (!data.is_null()).then_some(data),
            );
            emit_status(&status);
            Ok(())
        })())
    }

    fn report_to_user_markdown(&self, markdown: &str) -> VmResult<()> {
        emit_report_to_user_markdown(markdown);
        VmResult::Ok(())
    }

    fn run_workflow(&self, workflow: Value, input: Value, options: Value) -> VmResult<Value> {
        vm_result_from_result(self.run_child_workflow(workflow, input, options))
    }

    fn run_child_workflow(&self, workflow: Value, input: Value, options: Value) -> Result<Value> {
        let workflow_id = workflow_id_from_value(workflow)?;
        let input = rune_value_to_json(input)?;
        let raw_input = serde_json::to_string(&input)?;
        let mut child = Command::new(&self.inner.self_exe)
            .args([
                "workflow",
                "run",
                workflow_id.as_str(),
                "--input",
                raw_input.as_str(),
            ])
            .current_dir(&self.inner.working_directory)
            .env(
                WORKFLOW_WORKING_DIRECTORY_ENV,
                &self.inner.working_directory,
            )
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("failed to start child workflow {workflow_id}"))?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("child workflow stdout was not piped"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("child workflow stderr was not piped"))?;

        let stdout_task = std::thread::spawn(move || -> Result<String> {
            let mut stdout_text = String::new();
            std::io::Read::read_to_string(&mut std::io::BufReader::new(stdout), &mut stdout_text)?;
            Ok(stdout_text)
        });

        let status_hook = status_hook_from_options(&options);
        let raw_stderr = read_child_stderr(stderr, workflow_id, status_hook, self.clone())?;
        let status = child.wait().context("failed to wait for child workflow")?;
        let stdout = stdout_task
            .join()
            .map_err(|panic| anyhow!("child workflow stdout thread panicked: {panic:?}"))??;

        if !status.success() {
            if raw_stderr.trim().is_empty() {
                anyhow::bail!("child workflow exited with {status}");
            }
            anyhow::bail!("child workflow exited with {status}: {}", raw_stderr.trim());
        }

        if stdout.trim().is_empty() {
            return json_value_to_rune_result(JsonValue::Null);
        }
        serde_json::from_str::<Value>(stdout.trim()).context("child workflow stdout was not JSON")
    }

    fn working_directory(&self) -> String {
        self.inner.working_directory.clone()
    }

    fn app_server(&self) -> WorkflowRuneAppServer {
        self.inner.app_server.clone()
    }

    #[rune::function(keep, instance, path = Self::createAgent)]
    async fn create_agent(this: Ref<Self>, options: Value) -> VmResult<WorkflowRuneAgentHandle> {
        rune_app_server::vm_result_from_future(async {
            WorkflowRuneAgentHandle::create(this.inner.app_server.clone(), options).await
        })
        .await
    }

    #[rune::function(keep, instance, path = Self::startAgent)]
    async fn start_agent(this: Ref<Self>, options: Value) -> VmResult<WorkflowRuneAgentHandle> {
        Self::create_agent(this, options).await
    }

    #[rune::function(keep, instance, path = Self::resumeAgent)]
    async fn resume_agent(
        this: Ref<Self>,
        thread_id: Ref<str>,
        options: Value,
    ) -> VmResult<WorkflowRuneAgentHandle> {
        rune_app_server::vm_result_from_future(async {
            WorkflowRuneAgentHandle::resume(
                this.inner.app_server.clone(),
                thread_id.to_string(),
                options,
            )
            .await
        })
        .await
    }

    #[rune::function(keep, instance, path = Self::defineTool)]
    fn define_tool(
        this: Ref<Self>,
        spec: Value,
        handler: Function,
    ) -> VmResult<WorkflowRuneDynamicTool> {
        rune_app_server::vm_result_from_result(WorkflowRuneDynamicTool::define(
            this.inner.app_server.clone(),
            spec,
            handler,
        ))
    }

    fn input(&self) -> WorkflowRuneInputHelpers {
        WorkflowRuneInputHelpers
    }

    fn api(&self) -> WorkflowRuneApiNamespace {
        WorkflowRuneApiNamespace::new(self.inner.app_server.clone())
    }

    fn artifacts(&self) -> WorkflowRuneArtifactsNamespace {
        WorkflowRuneArtifactsNamespace::new(self.inner.app_server.clone())
    }

    fn workflows(&self) -> WorkflowRuneWorkflowsNamespace {
        WorkflowRuneWorkflowsNamespace::new(self.inner.app_server.clone())
    }

    fn mcp(&self) -> WorkflowRuneMcpNamespace {
        WorkflowRuneMcpNamespace::new(self.inner.app_server.clone())
    }

    fn tools(&self) -> WorkflowRuneToolsNamespace {
        WorkflowRuneToolsNamespace::new(self.inner.app_server.clone())
    }

    fn fs(&self) -> WorkflowRuneFsNamespace {
        WorkflowRuneFsNamespace::new(self.inner.app_server.clone())
    }

    fn process(&self) -> WorkflowRuneProcessNamespace {
        WorkflowRuneProcessNamespace::new(self.inner.app_server.clone())
    }
}

#[derive(Clone, Any)]
#[rune(item = ::codex)]
struct WorkflowRuneChildStatusHelpers {
    inner: Arc<WorkflowRuneChildStatusHelpersInner>,
}

struct WorkflowRuneChildStatusHelpersInner {
    original_child_status: WorkflowStatusUpdate,
    reported: AtomicBool,
}

impl WorkflowRuneChildStatusHelpers {
    fn new(original_child_status: WorkflowStatusUpdate) -> Self {
        Self {
            inner: Arc::new(WorkflowRuneChildStatusHelpersInner {
                original_child_status,
                reported: AtomicBool::new(false),
            }),
        }
    }

    fn report_status(&self, status: Value) -> VmResult<()> {
        vm_result_from_result((|| {
            let status = normalize_status_update(rune_value_to_json(status)?, "status")?;
            emit_status(&status);
            self.inner.reported.store(true, Ordering::SeqCst);
            Ok(())
        })())
    }

    fn attach_original_child_status(&self, status: Value) -> VmResult<Value> {
        vm_result_from_result((|| {
            let mut status = normalize_status_update(rune_value_to_json(status)?, "status")?;
            status.child_statuses.push(WorkflowChildStatus {
                workflow_name: self.inner.original_child_status.workflow_name.clone(),
                workflow_status: self.inner.original_child_status.workflow_status.clone(),
                threads: self.inner.original_child_status.threads.clone(),
            });
            json_value_to_rune_result(serde_json::to_value(status)?)
        })())
    }

    fn reported(&self) -> bool {
        self.inner.reported.load(Ordering::SeqCst)
    }
}

fn codex_module() -> Result<Module, rune::ContextError> {
    let mut module = Module::with_crate("codex")?;
    module.ty::<WorkflowRuneContext>()?;
    module.associated_function("status", WorkflowRuneContext::status)?;
    module.associated_function("progress", WorkflowRuneContext::progress)?;
    module.associated_function(
        "reportToUserMarkdown",
        WorkflowRuneContext::report_to_user_markdown,
    )?;
    module.associated_function("runWorkflow", WorkflowRuneContext::run_workflow)?;
    module.function_meta(WorkflowRuneContext::create_agent__meta)?;
    module.function_meta(WorkflowRuneContext::start_agent__meta)?;
    module.function_meta(WorkflowRuneContext::resume_agent__meta)?;
    module.function_meta(WorkflowRuneContext::define_tool__meta)?;
    WorkflowRuneAgentHandle::install(&mut module)?;
    WorkflowRuneDynamicTool::install(&mut module)?;
    WorkflowRuneInputHelpers::install(&mut module)?;
    WorkflowRuneApiNamespace::install(&mut module)?;
    WorkflowRuneArtifactsNamespace::install(&mut module)?;
    WorkflowRuneWorkflowsNamespace::install(&mut module)?;
    WorkflowRuneMcpNamespace::install(&mut module)?;
    WorkflowRuneToolsNamespace::install(&mut module)?;
    WorkflowRuneFsNamespace::install(&mut module)?;
    WorkflowRuneProcessNamespace::install(&mut module)?;
    WorkflowRuneAppServer::install(&mut module)?;
    module.field_function(
        &Protocol::GET,
        "cwd",
        WorkflowRuneContext::working_directory,
    )?;
    module.field_function(
        &Protocol::GET,
        "currentWorkingDirectory",
        WorkflowRuneContext::working_directory,
    )?;
    module.field_function(
        &Protocol::GET,
        "repoRoot",
        WorkflowRuneContext::working_directory,
    )?;
    module.field_function(
        &Protocol::GET,
        "workingDirectory",
        WorkflowRuneContext::working_directory,
    )?;
    module.field_function(&Protocol::GET, "appServer", WorkflowRuneContext::app_server)?;
    module.field_function(&Protocol::GET, "input", WorkflowRuneContext::input)?;
    module.field_function(&Protocol::GET, "api", WorkflowRuneContext::api)?;
    module.field_function(&Protocol::GET, "artifacts", WorkflowRuneContext::artifacts)?;
    module.field_function(&Protocol::GET, "workflows", WorkflowRuneContext::workflows)?;
    module.field_function(&Protocol::GET, "mcp", WorkflowRuneContext::mcp)?;
    module.field_function(&Protocol::GET, "tools", WorkflowRuneContext::tools)?;
    module.field_function(&Protocol::GET, "fs", WorkflowRuneContext::fs)?;
    module.field_function(&Protocol::GET, "process", WorkflowRuneContext::process)?;

    module.ty::<WorkflowRuneChildStatusHelpers>()?;
    module.associated_function(
        "reportStatus",
        WorkflowRuneChildStatusHelpers::report_status,
    )?;
    module.associated_function(
        "attachOriginalChildStatus",
        WorkflowRuneChildStatusHelpers::attach_original_child_status,
    )?;
    Ok(module)
}

fn read_child_stderr(
    stderr: impl std::io::Read,
    workflow_id: String,
    status_hook: Option<Function>,
    parent: WorkflowRuneContext,
) -> Result<String> {
    let mut raw_stderr = String::new();
    for line in std::io::BufReader::new(stderr).lines() {
        let line = line.context("failed to read child workflow stderr")?;
        let Some(event) = parse_workflow_event_line(&line, &workflow_id)? else {
            raw_stderr.push_str(&line);
            raw_stderr.push('\n');
            continue;
        };
        let WorkflowRuntimeEvent::Status { status } = event else {
            continue;
        };
        match status_hook.as_ref() {
            Some(hook) => apply_status_hook(hook, &status)?,
            None => parent
                .status(json_value_to_rune_result(serde_json::to_value(status)?)?)
                .into_result()
                .map_err(anyhow::Error::from)?,
        }
    }
    Ok(raw_stderr)
}

fn apply_status_hook(hook: &Function, child_status: &WorkflowStatusUpdate) -> Result<()> {
    let helpers = WorkflowRuneChildStatusHelpers::new(child_status.clone());
    let decision = hook
        .call::<Value>((
            json_value_to_rune_result(serde_json::to_value(child_status)?)?,
            helpers.clone(),
        ))
        .into_result()
        .map_err(anyhow::Error::from)?;
    if decision.into_unit().is_ok() {
        if !helpers.reported() {
            emit_status(child_status);
        }
        return Ok(());
    }
    let decision = rune_value_to_json(decision)?;
    if decision.is_null() {
        return Ok(());
    }
    let status = normalize_status_update(decision, "status hook return value")?;
    emit_status(&status);
    Ok(())
}

fn status_hook_from_options(options: &Value) -> Option<Function> {
    let object = options.borrow_ref::<Object>().ok()?;
    let value = object.get("onStatusUpdate")?.clone();
    rune::from_value::<Function>(value).ok()
}

fn parse_workflow_event_line(
    line: &str,
    workflow_name: &str,
) -> Result<Option<WorkflowRuntimeEvent>> {
    let Some(payload) = line.strip_prefix(WORKFLOW_RUNTIME_EVENT_PREFIX) else {
        return Ok(None);
    };
    let event = serde_json::from_str::<WorkflowRuntimeEvent>(payload)
        .with_context(|| format!("failed to decode workflow runtime event `{payload}`"))?;
    Ok(Some(match event {
        WorkflowRuntimeEvent::Progress { message, data } => WorkflowRuntimeEvent::Status {
            status: legacy_progress_to_status(workflow_name, &message, data),
        },
        other => other,
    }))
}

fn normalize_completion_suggestions(
    value: Value,
) -> Result<Vec<WorkflowCommandCompletionSuggestion>> {
    let value = rune_value_to_json(value)?;
    let Some(entries) = value.as_array() else {
        return Ok(Vec::new());
    };
    entries
        .iter()
        .map(normalize_completion_suggestion)
        .collect()
}

fn normalize_completion_suggestion(
    value: &JsonValue,
) -> Result<WorkflowCommandCompletionSuggestion> {
    if let Some(value) = value
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Ok(WorkflowCommandCompletionSuggestion {
            display: value.to_string(),
            insert_text: value.to_string(),
            description: None,
        });
    }
    let Some(object) = value.as_object() else {
        anyhow::bail!("workflow completion entries must be strings or objects");
    };
    let display = string_value(object.get("display"))
        .or_else(|| string_value(object.get("insertText")))
        .ok_or_else(|| {
            anyhow!("workflow completion entries require non-empty display or insertText")
        })?;
    let insert_text = string_value(object.get("insertText"))
        .or_else(|| string_value(object.get("display")))
        .ok_or_else(|| {
            anyhow!("workflow completion entries require non-empty display or insertText")
        })?;
    Ok(WorkflowCommandCompletionSuggestion {
        display,
        insert_text,
        description: string_value(object.get("description")),
    })
}

fn workflow_id_from_value(value: Value) -> Result<String> {
    match rune_value_to_json(value)? {
        JsonValue::String(id) if !id.trim().is_empty() => Ok(id),
        JsonValue::Object(object) => object
            .get("id")
            .and_then(JsonValue::as_str)
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(ToString::to_string)
            .ok_or_else(|| {
                anyhow!("ctx.runWorkflow(workflow, ...) requires a workflow id string or {{ id }}")
            }),
        _ => anyhow::bail!(
            "ctx.runWorkflow(workflow, ...) requires a workflow id string or {{ id }}"
        ),
    }
}

fn legacy_progress_to_status(
    workflow_name: &str,
    message: &str,
    data: Option<JsonValue>,
) -> WorkflowStatusUpdate {
    let trimmed_message = message.trim();
    let summary = data.as_ref().and_then(format_legacy_progress_data);
    let workflow_status = match (trimmed_message.is_empty(), summary) {
        (false, Some(summary)) => format!("{trimmed_message} ({summary})"),
        (false, None) => trimmed_message.to_string(),
        (true, Some(summary)) => summary,
        (true, None) => "running".to_string(),
    };
    WorkflowStatusUpdate {
        workflow_name: workflow_name.to_string(),
        workflow_status,
        threads: Vec::new(),
        child_statuses: Vec::new(),
    }
}

fn format_legacy_progress_data(data: &JsonValue) -> Option<String> {
    let object = data.as_object()?;
    let mut parts = Vec::new();
    if let Some(stage) = scalar_value(object.get("stage")) {
        parts.push(stage);
    }
    let step = scalar_value(object.get("step"));
    let total = scalar_value(object.get("total"));
    match (step, total) {
        (Some(step), Some(total)) => parts.push(format!("step {step}/{total}")),
        (Some(step), None) => parts.push(format!("step {step}")),
        _ => {}
    }
    for (key, value) in object {
        if matches!(key.as_str(), "stage" | "step" | "total") {
            continue;
        }
        if let Some(scalar) = scalar_value(Some(value)) {
            parts.push(format!("{key} {scalar}"));
        }
    }
    (!parts.is_empty()).then(|| parts.join(", "))
}

fn normalize_status_update(value: JsonValue, label: &str) -> Result<WorkflowStatusUpdate> {
    let object = value
        .as_object()
        .ok_or_else(|| anyhow!("{label} must be an object"))?;
    let workflow_name = string_value(object.get("workflowName"))
        .or_else(|| string_value(object.get("name")))
        .ok_or_else(|| anyhow!("{label} requires non-empty workflowName and workflowStatus"))?;
    let workflow_status = string_value(object.get("workflowStatus"))
        .or_else(|| string_value(object.get("status")))
        .ok_or_else(|| anyhow!("{label} requires non-empty workflowName and workflowStatus"))?;
    let threads = object
        .get("threads")
        .and_then(JsonValue::as_array)
        .map(|threads| {
            threads
                .iter()
                .enumerate()
                .map(|(index, thread)| normalize_thread_status(thread, index))
                .collect::<Result<Vec<_>>>()
        })
        .transpose()?
        .unwrap_or_default();
    let child_statuses = object
        .get("childStatuses")
        .and_then(JsonValue::as_array)
        .map(|children| {
            children
                .iter()
                .enumerate()
                .map(|(index, child)| normalize_child_status(child, index))
                .collect::<Result<Vec<_>>>()
        })
        .transpose()?
        .unwrap_or_default();
    Ok(WorkflowStatusUpdate {
        workflow_name,
        workflow_status,
        threads,
        child_statuses,
    })
}

fn normalize_thread_status(value: &JsonValue, index: usize) -> Result<WorkflowThreadStatus> {
    let object = value
        .as_object()
        .ok_or_else(|| anyhow!("workflow thread status at index {index} must be an object"))?;
    let name = string_value(object.get("name")).ok_or_else(|| {
        anyhow!("workflow thread status at index {index} requires non-empty name and status")
    })?;
    let status = string_value(object.get("status")).ok_or_else(|| {
        anyhow!("workflow thread status at index {index} requires non-empty name and status")
    })?;
    Ok(WorkflowThreadStatus { name, status })
}

fn normalize_child_status(value: &JsonValue, index: usize) -> Result<WorkflowChildStatus> {
    let status = normalize_status_update(value.clone(), &format!("childStatuses[{index}]"))?;
    Ok(WorkflowChildStatus {
        workflow_name: status.workflow_name,
        workflow_status: status.workflow_status,
        threads: status.threads,
    })
}

fn emit_status(status: &WorkflowStatusUpdate) {
    emit_runtime_event(json!({ "type": "status", "status": status }));
}

fn emit_report_to_user_markdown(markdown: &str) {
    emit_runtime_event(json!({ "type": "reportToUserMarkdown", "markdown": markdown }));
}

fn emit_runtime_event(event: JsonValue) {
    match serde_json::to_string(&event) {
        Ok(event) => eprintln!("{WORKFLOW_RUNTIME_EVENT_PREFIX}{event}"),
        Err(err) => eprintln!("failed to encode workflow runtime event: {err}"),
    }
}

fn runtime_error_output(error: anyhow::Error) -> WorkflowRuntimeOutput {
    WorkflowRuntimeOutput {
        stdout: String::new(),
        stderr: format!("{error:#}"),
        success: false,
        exit_status: "rune runtime error".to_string(),
    }
}

fn string_value(value: Option<&JsonValue>) -> Option<String> {
    value
        .and_then(JsonValue::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn scalar_value(value: Option<&JsonValue>) -> Option<String> {
    match value? {
        JsonValue::String(value) => Some(value.clone()),
        JsonValue::Number(value) => Some(value.to_string()),
        JsonValue::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

fn rune_value_to_json(value: Value) -> Result<JsonValue> {
    serde_json::to_value(&value).context("failed to convert Rune value to JSON")
}

fn json_value_to_rune_result(value: JsonValue) -> Result<Value> {
    serde_json::from_value::<Value>(value).context("failed to convert JSON value to Rune")
}

fn vm_result_from_result<T>(result: Result<T>) -> VmResult<T> {
    match result {
        Ok(value) => VmResult::Ok(value),
        Err(err) => VmResult::panic(format!("{err:#}")),
    }
}

fn is_missing_rune_function(error: &anyhow::Error) -> bool {
    let error = error.to_string();
    error.contains("Missing entry") || error.contains("missing entry")
}

#[cfg(test)]
mod tests {
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    use pretty_assertions::assert_eq;
    use serde_json::json;
    use serial_test::serial;
    use tempfile::TempDir;

    use super::complete_workflow;
    use super::run_workflow;
    use crate::WorkflowCommandCompletionSuggestion;
    use crate::WorkflowCommandInput;
    use crate::workflow_runtime::WORKFLOW_SELF_EXE_ENV;

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<std::ffi::OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
            let previous = std::env::var_os(key);
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            unsafe {
                match &self.previous {
                    Some(value) => std::env::set_var(self.key, value),
                    None => std::env::remove_var(self.key),
                }
            }
        }
    }

    fn write_workflow(
        temp: &TempDir,
        name: &str,
        source: &str,
    ) -> (std::path::PathBuf, std::path::PathBuf) {
        let workflow_dir = temp.path().join(name);
        fs::create_dir_all(workflow_dir.join("src")).expect("workflow src");
        let workflow_path = workflow_dir.join("src/workflow.rn");
        fs::write(&workflow_path, source).expect("workflow source");
        (workflow_dir, workflow_path)
    }

    #[cfg(unix)]
    fn write_executable(path: &Path, source: &str) {
        fs::write(path, source).expect("fake executable");
        let mut permissions = fs::metadata(path).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).expect("chmod");
    }

    #[tokio::test]
    async fn run_workflow_executes_rune_entrypoint() {
        let temp = TempDir::new().expect("temp dir");
        let workflow_dir = temp.path().join("rune-workflow");
        fs::create_dir_all(workflow_dir.join("src")).expect("workflow src");
        let workflow_path = workflow_dir.join("src/workflow.rn");
        fs::write(
            &workflow_path,
            r#"
pub async fn run(ctx, input) {
    ctx.status(#{ workflowName: "Rune", workflowStatus: "running", threads: [] });
    ctx.progress("Working", #{ step: 1, total: 2 });
    #{ ok: true, input }
}
"#,
        )
        .expect("workflow source");

        let output = run_workflow(
            temp.path(),
            &workflow_dir,
            &workflow_path,
            r#"{ "value": "example" }"#,
        )
        .await
        .expect("run workflow");

        assert!(output.success, "stderr: {}", output.stderr);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&output.stdout).expect("json stdout"),
            json!({ "ok": true, "input": { "value": "example" } })
        );
    }

    #[tokio::test]
    async fn complete_workflow_executes_rune_complete_hook() {
        let temp = TempDir::new().expect("temp dir");
        let workflow_dir = temp.path().join("rune-workflow");
        fs::create_dir_all(workflow_dir.join("src")).expect("workflow src");
        let workflow_path = workflow_dir.join("src/workflow.rn");
        fs::write(
            &workflow_path,
            r#"
pub async fn run(_ctx, input) {
    input
}

pub async fn complete(_ctx, input) {
    if input.text == "--format" {
        [
            "--format summary",
            #{ display: "--id 123", insertText: "--id 123", description: "Workflow" },
        ]
    } else {
        []
    }
}
"#,
        )
        .expect("workflow source");

        let suggestions = complete_workflow(
            &workflow_dir,
            temp.path(),
            &workflow_path,
            &WorkflowCommandInput {
                argv: vec!["--format".to_string()],
                text: "--format".to_string(),
            },
        )
        .await
        .expect("complete workflow");

        assert_eq!(
            suggestions,
            vec![
                WorkflowCommandCompletionSuggestion {
                    display: "--format summary".to_string(),
                    insert_text: "--format summary".to_string(),
                    description: None,
                },
                WorkflowCommandCompletionSuggestion {
                    display: "--id 123".to_string(),
                    insert_text: "--id 123".to_string(),
                    description: Some("Workflow".to_string()),
                },
            ]
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    #[serial]
    async fn run_workflow_exercises_agent_handles_tools_and_ux_calls_e2e() {
        let temp = TempDir::new().expect("temp dir");
        let fake_codex = temp.path().join("fake-codex-agent");
        write_executable(
            &fake_codex,
            r#"#!/usr/bin/env python3
import json
import sys

pending_turns = {}
thread_counter = 0
turn_counter = 0

def send(payload):
    print(json.dumps(payload, separators=(",", ":")), flush=True)

for line in sys.stdin:
    message = json.loads(line)
    method = message.get("method")
    request_id = message.get("id")

    if method == "initialize":
        send({"id": request_id, "result": {"userAgent": "fake/1", "serverInfo": {"name": "fake", "version": "1"}}})
    elif method == "initialized":
        pass
    elif method == "thread/start":
        thread_counter += 1
        if not message.get("params", {}).get("dynamicTools"):
            send({"id": request_id, "error": {"code": -32000, "message": "missing dynamicTools"}})
        else:
            send({"id": request_id, "result": {"thread": {"id": f"thread-{thread_counter}"}}})
    elif method == "thread/resume":
        send({"id": request_id, "result": {"thread": {"id": message["params"]["threadId"]}}})
    elif method == "thread/fork":
        send({"id": request_id, "result": {"thread": {"id": "thread-fork"}}})
    elif method == "thread/read":
        send({"id": request_id, "result": {"thread": {"id": message["params"]["threadId"], "status": "idle"}}})
    elif method == "thread/unsubscribe":
        send({"id": request_id, "result": {"unsubscribed": message["params"]["threadId"]}})
    elif method == "turn/start":
        turn_counter += 1
        turn_id = f"turn-{turn_counter}"
        tool_request_id = f"tool-call-{turn_counter}"
        pending_turns[tool_request_id] = {
            "request_id": request_id,
            "thread_id": message["params"]["threadId"],
            "turn_id": turn_id,
        }
        send({
            "id": tool_request_id,
            "method": "item/tool/call",
            "params": {
                "tool": "echo_context",
                "arguments": {"text": f"tool-input-{turn_counter}"},
                "callId": f"call-{turn_counter}",
                "threadId": message["params"]["threadId"],
                "turnId": turn_id,
            },
        })
    elif method == "turn/steer":
        send({"id": request_id, "result": {"steered": True, "input": message["params"]["input"]}})
    elif method == "turn/interrupt":
        send({"id": request_id, "result": {"interrupted": message["params"]["turnId"]}})
    elif request_id in pending_turns:
        pending = pending_turns.pop(request_id)
        result = message.get("result", {})
        content_items = result.get("contentItems") or [{}]
        tool_text = content_items[0].get("text", "missing-tool-result")
        thread_id = pending["thread_id"]
        turn_id = pending["turn_id"]
        send({
            "method": "item/completed",
            "params": {
                "threadId": thread_id,
                "turnId": turn_id,
                "item": {"type": "agentMessage", "id": f"item-{turn_id}", "text": f"agent saw {tool_text}"},
            },
        })
        send({"method": "thread/tokenUsage/updated", "params": {"threadId": thread_id, "tokenUsage": {"last": {"totalTokens": 42}}}})
        send({"method": "turn/completed", "params": {"threadId": thread_id, "turn": {"id": turn_id, "items": [], "status": "completed"}}})
        send({"id": pending["request_id"], "result": {"turn": {"id": turn_id, "items": [], "status": "inProgress"}}})
"#,
        );
        let _env = EnvVarGuard::set(WORKFLOW_SELF_EXE_ENV, &fake_codex);
        let (workflow_dir, workflow_path) = write_workflow(
            &temp,
            "rune-agent-e2e",
            r##"
pub async fn run(ctx, input) {
    ctx.appServer.configure(#{ connection: "spawn" });
    ctx.status(#{
        workflowName: "Rune Agent E2E",
        workflowStatus: "starting",
        threads: [
            #{ name: "planner", status: "running" },
            #{ name: "executor", status: "waiting" },
        ],
    });
    ctx.progress("Preparing agent", #{ stage: "setup", step: 1, total: 2 });
    ctx.reportToUserMarkdown("# Rune Agent E2E");

    let echo = ctx.defineTool(#{
        name: "echo_context",
        description: "Echoes text to prove dynamic tool calls are routed through Rune.",
        inputSchema: #{
            type: "object",
            properties: #{ text: #{ type: "string" } },
            required: ["text"],
        },
    }, async |args, _call| {
        args.text
    });

    let agent = ctx.startAgent(#{ tools: [echo] }).await;
    let forked = agent.fork(#{ tools: [echo] }).await;
    let spawned = agent.spawnAgent(#{ tools: [echo] }).await;
    let created = agent.createAgent(#{ tools: [echo] }).await;
    let resumed = ctx.resumeAgent("thread-resumed", #{ tools: [echo] }).await;
    let wait_result = agent.wait().await;

    let turn = agent.turn([
        ctx.input.text(input.prompt),
        ctx.input.image("https://example.com/image.png"),
        ctx.input.localImage("local.png"),
        ctx.input.skill("review", "/skills/review"),
        ctx.input.mention("file", "/tmp/file.txt"),
    ], #{}).await;
    let steer_result = turn.steer("steer this turn").await;
    let interrupt_result = turn.interrupt().await;
    let run_result = turn.run().await;

    let stream = forked.runStreamed("stream this turn", #{}).await;
    let stream_item = stream.next().await;
    let stream_usage = stream.next().await;
    let stream_done = stream.next().await;
    stream.close().await;

    let send_turn = resumed.sendInput("send input turn", #{}).await;
    let send_result = send_turn.run().await;
    let unsubscribe_result = agent.unsubscribe().await;
    let close_result = spawned.close().await;
    let created_close_result = created.close().await;

    #{
        agentThreadId: agent.threadId,
        forkedThreadId: forked.threadId,
        spawnedThreadId: spawned.threadId,
        createdThreadId: created.threadId,
        resumedThreadId: resumed.threadId,
        waitResult: wait_result,
        steerResult: steer_result,
        interruptResult: interrupt_result,
        runResult: run_result,
        streamMethods: [stream_item.method, stream_usage.method, stream_done.method],
        streamFinal: stream_item.params.item.text,
        sendResult: send_result,
        unsubscribeResult: unsubscribe_result,
        closeResult: close_result,
        createdCloseResult: created_close_result,
    }
}
"##,
        );

        let output = run_workflow(
            temp.path(),
            &workflow_dir,
            &workflow_path,
            r#"{ "prompt": "inspect everything" }"#,
        )
        .await
        .expect("run workflow");

        assert!(output.success, "stderr: {}", output.stderr);
        let result: serde_json::Value = serde_json::from_str(&output.stdout).expect("json stdout");
        assert_eq!(result["agentThreadId"], json!("thread-1"));
        assert_eq!(result["forkedThreadId"], json!("thread-fork"));
        assert_eq!(result["spawnedThreadId"], json!("thread-2"));
        assert_eq!(result["createdThreadId"], json!("thread-3"));
        assert_eq!(result["resumedThreadId"], json!("thread-resumed"));
        assert_eq!(result["waitResult"]["thread"]["status"], json!("idle"));
        assert_eq!(result["steerResult"]["steered"], json!(true));
        assert_eq!(result["interruptResult"]["interrupted"], json!("turn-1"));
        assert_eq!(
            result["runResult"]["finalResponse"],
            json!("agent saw tool-input-1")
        );
        assert_eq!(
            result["runResult"]["usage"]["last"]["totalTokens"],
            json!(42)
        );
        assert_eq!(
            result["streamMethods"],
            json!([
                "item/completed",
                "thread/tokenUsage/updated",
                "turn/completed"
            ])
        );
        assert_eq!(result["streamFinal"], json!("agent saw tool-input-2"));
        assert_eq!(
            result["sendResult"]["finalResponse"],
            json!("agent saw tool-input-3")
        );
        assert_eq!(
            result["unsubscribeResult"]["unsubscribed"],
            json!("thread-1")
        );
        assert_eq!(result["closeResult"]["unsubscribed"], json!("thread-2"));
        assert_eq!(
            result["createdCloseResult"]["unsubscribed"],
            json!("thread-3")
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    #[serial]
    async fn run_workflow_exercises_app_server_namespaces_e2e() {
        let temp = TempDir::new().expect("temp dir");
        let fake_codex = temp.path().join("fake-codex-api");
        write_executable(
            &fake_codex,
            r#"#!/usr/bin/env python3
import json
import sys

pending_approval_request = None
overload_attempts = 0

def send(payload):
    print(json.dumps(payload, separators=(",", ":")), flush=True)

for line in sys.stdin:
    message = json.loads(line)
    method = message.get("method")
    request_id = message.get("id")

    if method == "initialize":
        send({"id": request_id, "result": {"userAgent": "fake/1", "serverInfo": {"name": "fake", "version": "1"}}})
    elif method == "initialized":
        pass
    elif method == "test/notify":
        send({"method": "test/notification", "params": {"seen": True}})
    elif method == "test/error":
        send({"id": request_id, "error": {"code": 429, "message": "overloaded"}})
    elif method == "test/overload":
        overload_attempts += 1
        if overload_attempts == 1:
            send({"id": request_id, "error": {"code": 429, "message": "overloaded"}})
        else:
            send({"id": request_id, "result": {"method": method, "attempts": overload_attempts}})
    elif method == "test/approval":
        pending_approval_request = request_id
        send({
            "id": "approval-request-1",
            "method": "item/commandExecution/requestApproval",
            "params": {"command": ["echo", "ok"]},
        })
    elif request_id == "approval-request-1":
        send({"id": pending_approval_request, "result": {"approval": message.get("result")}})
        pending_approval_request = None
    elif request_id is not None:
        send({"id": request_id, "result": {"method": method, "params": message.get("params", {})}})
"#,
        );
        let _env = EnvVarGuard::set(WORKFLOW_SELF_EXE_ENV, &fake_codex);
        let (workflow_dir, workflow_path) = write_workflow(
            &temp,
            "rune-api-e2e",
            r##"
pub async fn run(ctx, _input) {
    ctx.appServer.configure(#{ connection: "spawn" });

    let api = ctx.api.read(#{}).await;
    let artifact_register = ctx.artifacts.registerState(#{ id: "state-1" }).await;
    let artifact_read = ctx.artifacts.readState(#{ id: "state-1" }).await;
    let artifact_list = ctx.artifacts.listStates(#{}).await;
    let artifact_hit = ctx.artifacts.recordStateHit(#{ id: "state-1" }).await;
    let artifact_prune = ctx.artifacts.pruneStates(#{}).await;
    let artifact_index = ctx.artifacts.indexFile(#{ path: "README.md" }).await;
    let artifact_find = ctx.artifacts.findFile(#{ path: "README.md" }).await;
    let cache_read = ctx.artifacts.readCacheEntry(#{ key: "cache-1" }).await;
    let cache_write = ctx.artifacts.writeCacheEntry(#{ key: "cache-1", value: #{ ok: true } }).await;
    let cache_delete = ctx.artifacts.deleteCacheEntry(#{ key: "cache-1" }).await;

    let workflow_list = ctx.workflows.registry.list(#{}).await;
    let workflow_read = ctx.workflows.registry.read(#{ id: "demo" }).await;
    let workflow_impact = ctx.workflows.registry.impact(#{ id: "demo" }).await;
    let workflow_develop = ctx.workflows.registry.develop(#{ description: "demo" }).await;
    let workflow_edit = ctx.workflows.registry.edit(#{ id: "demo" }).await;
    let workflow_validate = ctx.workflows.registry.validate(#{ id: "demo" }).await;
    let workflow_repair = ctx.workflows.registry.repair(#{ id: "demo" }).await;
    let workflow_author = ctx.workflows.registry.authoringContextPrepare(#{ id: "demo" }).await;
    let workflow_config_read = ctx.workflows.config.read(#{}).await;
    let workflow_config_write = ctx.workflows.config.write(#{ key: "default_location" }).await;
    let workflow_command = ctx.workflows.command.execute(#{ argv: ["list"] }).await;
    let workflow_run = ctx.workflows.run("demo", #{ prompt: "hello" }).await;

    let mcp_servers = ctx.mcp.listServers(#{}).await;
    let mcp_resource = ctx.mcp.readResource(#{ server: "s", uri: "file://demo" }).await;
    let mcp_tool = ctx.mcp.callTool(#{ server: "s", tool: "t", arguments: #{} }).await;
    let exec = ctx.tools.exec(["echo", "hi"], #{ cwd: ctx.cwd }).await;

    let fs_read = ctx.fs.readFile(#{ path: "README.md" }).await;
    let fs_write = ctx.fs.writeFile(#{ path: "out.txt", content: "hi" }).await;
    let fs_dir = ctx.fs.readDirectory(#{ path: ctx.cwd }).await;
    let fs_mkdir = ctx.fs.createDirectory(#{ path: "tmp" }).await;
    let fs_remove = ctx.fs.remove(#{ path: "tmp" }).await;
    let fs_copy = ctx.fs.copy(#{ from: "a", to: "b" }).await;
    let fs_watch = ctx.fs.watch(#{ path: ctx.cwd }).await;
    let fs_unwatch = ctx.fs.unwatch(#{ watchId: "watch-1" }).await;

    let process_spawn = ctx.process.spawn(#{ command: ["cat"] }).await;
    let process_stdin = ctx.process.writeStdin(#{ processId: "p", data: "hi" }).await;
    let process_kill = ctx.process.kill(#{ processId: "p" }).await;
    let process_resize = ctx.process.resizePty(#{ processId: "p", cols: 80, rows: 24 }).await;

    let raw = ctx.appServer.request("test/raw", #{ value: 1 }).await;
    let try_ok = ctx.appServer.tryRequest("test/raw", #{ value: 2 }).await;
    let try_err = ctx.appServer.tryRequest("test/error", #{}).await;
    ctx.appServer.notify("test/notify", #{ value: 3 }).await;
    let notification = ctx.appServer.nextNotification().await;
    let retry = ctx.appServer.retryOnOverload(async || {
        ctx.appServer.request("test/overload", #{}).await
    }, #{ maxRetries: 2, maxDelayMs: 1 }).await;
    ctx.appServer.setApprovals(#{
        mode: "handler",
        onApproval: async |approval| {
            #{ decision: "approved", type: approval.type, method: approval.method }
        },
    });
    let approval = ctx.appServer.request("test/approval", #{}).await;
    let connected = ctx.appServer.connected;
    ctx.appServer.close().await;

    #{
        methods: [
            api.method,
            artifact_register.method,
            artifact_read.method,
            artifact_list.method,
            artifact_hit.method,
            artifact_prune.method,
            artifact_index.method,
            artifact_find.method,
            cache_read.method,
            cache_write.method,
            cache_delete.method,
            workflow_list.method,
            workflow_read.method,
            workflow_impact.method,
            workflow_develop.method,
            workflow_edit.method,
            workflow_validate.method,
            workflow_repair.method,
            workflow_author.method,
            workflow_config_read.method,
            workflow_config_write.method,
            workflow_command.method,
            workflow_run.method,
            mcp_servers.method,
            mcp_resource.method,
            mcp_tool.method,
            exec.method,
            fs_read.method,
            fs_write.method,
            fs_dir.method,
            fs_mkdir.method,
            fs_remove.method,
            fs_copy.method,
            fs_watch.method,
            fs_unwatch.method,
            process_spawn.method,
            process_stdin.method,
            process_kill.method,
            process_resize.method,
            raw.method,
        ],
        execParams: exec.params,
        tryOk: try_ok,
        tryErr: try_err,
        notification: notification,
        retry: retry,
        approval: approval,
        connected: connected,
    }
}
"##,
        );

        let output = run_workflow(temp.path(), &workflow_dir, &workflow_path, "{}")
            .await
            .expect("run workflow");

        assert!(output.success, "stderr: {}", output.stderr);
        let result: serde_json::Value = serde_json::from_str(&output.stdout).expect("json stdout");
        assert_eq!(
            result["methods"],
            json!([
                "apiCatalog/read",
                "artifact/state/register",
                "artifact/state/read",
                "artifact/state/list",
                "artifact/state/hit",
                "artifact/state/prune",
                "artifact/file/index",
                "artifact/file/find",
                "artifact/cache/read",
                "artifact/cache/write",
                "artifact/cache/delete",
                "workflow/list",
                "workflow/read",
                "workflow/impact",
                "workflow/develop",
                "workflow/edit",
                "workflow/validate",
                "workflow/repair",
                "workflow/authoringContext/prepare",
                "workflow/config/read",
                "workflow/config/write",
                "workflow/command/execute",
                "workflow/run",
                "mcpServerStatus/list",
                "mcpServer/resource/read",
                "mcpServer/tool/call",
                "command/exec",
                "fs/readFile",
                "fs/writeFile",
                "fs/readDirectory",
                "fs/createDirectory",
                "fs/remove",
                "fs/copy",
                "fs/watch",
                "fs/unwatch",
                "process/spawn",
                "process/writeStdin",
                "process/kill",
                "process/resizePty",
                "test/raw",
            ])
        );
        assert_eq!(result["execParams"]["command"], json!(["echo", "hi"]));
        assert_eq!(result["tryOk"]["ok"], json!(true));
        assert_eq!(result["tryErr"]["ok"], json!(false));
        assert_eq!(result["notification"]["method"], json!("test/notification"));
        assert_eq!(result["retry"]["attempts"], json!(2));
        assert_eq!(
            result["approval"]["approval"]["decision"],
            json!("approved")
        );
        assert_eq!(result["approval"]["approval"]["type"], json!("command"));
        assert_eq!(result["connected"], json!(true));
    }
}
