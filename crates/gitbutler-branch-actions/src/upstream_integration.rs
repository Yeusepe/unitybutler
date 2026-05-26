use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::{self, BufRead, BufReader, Read, Write},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context as _, Result, bail};
use bstr::ByteSlice;
use but_core::{RefMetadata, Reference, RepositoryExt, WORKSPACE_REF_NAME, ref_metadata::StackId};
use but_ctx::{Context, access::RepoExclusive};
use but_graph::FirstParent;
use but_rebase::{RebaseOutput, RebaseStep};
use but_serde::BStringForFrontend;
use but_workspace::{legacy::stack_ext::StackDetailsExt, ref_info::Options};
use gitbutler_commit::commit_ext::CommitExt as _;
use gitbutler_repo::{first_parent_commit_ids_until, rebase::merge_commits};
use gitbutler_stack::VirtualBranchesHandle;
use gitbutler_workspace::branch_trees::{WorkspaceState, update_uncommitted_changes};
use gix::merge::tree::TreatAsUnresolved;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::BranchManagerExt;

#[derive(Serialize, PartialEq, Debug)]
#[cfg_attr(feature = "export-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct NameAndStatus {
    pub name: String,
    pub status: BranchStatus,
}
#[cfg(feature = "export-schema")]
but_schemars::register_sdk_type!(NameAndStatus);

#[derive(Serialize, PartialEq, Debug)]
#[cfg_attr(feature = "export-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct StackStatus {
    pub tree_status: UpstreamTreeStatus,
    pub branch_statuses: Vec<NameAndStatus>,
}
#[cfg(feature = "export-schema")]
but_schemars::register_sdk_type!(StackStatus);

#[derive(Serialize, PartialEq, Debug)]
#[cfg_attr(feature = "export-schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", content = "subject", rename_all = "camelCase")]
pub enum UpstreamTreeStatus {
    SafelyUpdatable,
    Conflicted,
    Empty,
}
#[cfg(feature = "export-schema")]
but_schemars::register_sdk_type!(UpstreamTreeStatus);

#[derive(Serialize, PartialEq, Debug)]
#[cfg_attr(feature = "export-schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", content = "subject", rename_all = "camelCase")]
pub enum BranchStatus {
    SafelyUpdatable,
    Integrated,
    Conflicted {
        /// If the branch can be rebased onto the target without conflicts
        rebasable: bool,
    },
    Empty,
}
#[cfg(feature = "export-schema")]
but_schemars::register_sdk_type!(BranchStatus);

#[derive(Serialize, PartialEq, Debug)]
#[cfg_attr(feature = "export-schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", content = "subject", rename_all = "camelCase")]
pub enum StackStatuses {
    UpToDate,
    UpdatesRequired {
        #[serde(rename = "worktreeConflicts")]
        #[cfg_attr(feature = "export-schema", schemars(with = "Vec<String>"))]
        worktree_conflicts: Vec<BStringForFrontend>,
        #[cfg_attr(
            feature = "export-schema",
            schemars(with = "Vec<(Option<String>, StackStatus)>")
        )]
        statuses: Vec<(Option<StackId>, StackStatus)>,
    },
}
#[cfg(feature = "export-schema")]
but_schemars::register_sdk_type!(StackStatuses);

#[derive(Serialize, PartialEq, Debug)]
#[cfg_attr(feature = "export-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct WorktreeConflictPreview {
    pub path: String,
    pub mode: WorktreeConflictPreviewMode,
    pub session_id: String,
    pub lfs: UnityConflictLfsInfo,
    pub document: Option<UnityConflictPreviewDocument>,
    pub available_choices: Vec<UnityConflictSide>,
    pub message: Option<String>,
}
#[cfg(feature = "export-schema")]
but_schemars::register_sdk_type!(WorktreeConflictPreview);

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone, Copy)]
#[cfg_attr(feature = "export-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum WorktreeConflictPreviewMode {
    MergePreview,
    ChooseSide,
}
#[cfg(feature = "export-schema")]
but_schemars::register_sdk_type!(WorktreeConflictPreviewMode);

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone, Copy, Eq, Hash)]
#[cfg_attr(feature = "export-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum UnityConflictSide {
    Local,
    Upstream,
}
#[cfg(feature = "export-schema")]
but_schemars::register_sdk_type!(UnityConflictSide);

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
#[cfg_attr(feature = "export-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct UnityConflictLfsInfo {
    pub tracked: bool,
    pub base: UnityConflictSideState,
    pub local: UnityConflictSideState,
    pub upstream: UnityConflictSideState,
}
#[cfg(feature = "export-schema")]
but_schemars::register_sdk_type!(UnityConflictLfsInfo);

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
#[cfg_attr(feature = "export-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct UnityConflictSideState {
    pub state: UnityConflictMaterializeState,
    pub size: Option<u64>,
}
#[cfg(feature = "export-schema")]
but_schemars::register_sdk_type!(UnityConflictSideState);

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone, Copy)]
#[cfg_attr(feature = "export-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum UnityConflictMaterializeState {
    TextReady,
    MissingLfsObject,
    BinaryOrNonUtf8,
    PointerStillPresent,
    Absent,
}
#[cfg(feature = "export-schema")]
but_schemars::register_sdk_type!(UnityConflictMaterializeState);

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
#[cfg_attr(feature = "export-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct UnityConflictPreviewDocument {
    pub path: String,
    pub blocks: Vec<UnityConflictPreviewBlock>,
}
#[cfg(feature = "export-schema")]
but_schemars::register_sdk_type!(UnityConflictPreviewDocument);

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
#[cfg_attr(feature = "export-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct UnityConflictPreviewBlock {
    pub id: String,
    pub label: String,
    pub context: String,
    pub ours: String,
    pub theirs: String,
    pub fields: Vec<UnityConflictPreviewField>,
}
#[cfg(feature = "export-schema")]
but_schemars::register_sdk_type!(UnityConflictPreviewBlock);

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
#[cfg_attr(feature = "export-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct UnityConflictPreviewField {
    pub id: String,
    pub label: String,
    pub ours: String,
    pub theirs: String,
}
#[cfg(feature = "export-schema")]
but_schemars::register_sdk_type!(UnityConflictPreviewField);

#[derive(Serialize, Deserialize, PartialEq, Debug)]
#[cfg_attr(feature = "export-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct UnityConflictResolutionInput {
    pub session_id: String,
    pub path: String,
    pub resolution: UnityConflictResolution,
}
#[cfg(feature = "export-schema")]
but_schemars::register_sdk_type!(UnityConflictResolutionInput);

#[derive(Serialize, Deserialize, PartialEq, Debug)]
#[cfg_attr(feature = "export-schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum UnityConflictResolution {
    Blocks { blocks: HashMap<String, String> },
    Local,
    Upstream,
}
#[cfg(feature = "export-schema")]
but_schemars::register_sdk_type!(UnityConflictResolution);

#[derive(Serialize, Deserialize, PartialEq, Debug)]
#[cfg_attr(feature = "export-schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", content = "subject", rename_all = "camelCase")]
pub enum BaseBranchResolutionApproach {
    Rebase,
    Merge,
    HardReset,
}
#[cfg(feature = "export-schema")]
but_schemars::register_sdk_type!(BaseBranchResolutionApproach);

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "export-schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", content = "subject", rename_all = "camelCase")]
pub enum ResolutionApproach {
    Rebase,
    Merge,
    Unapply,
    Delete,
}
#[cfg(feature = "export-schema")]
but_schemars::register_sdk_type!(ResolutionApproach);

#[derive(Serialize, Deserialize, PartialEq, Debug)]
#[cfg_attr(feature = "export-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct BaseBranchResolution {
    #[serde(with = "but_serde::object_id")]
    #[cfg_attr(
        feature = "export-schema",
        schemars(schema_with = "but_schemars::object_id")
    )]
    target_commit_oid: gix::ObjectId,
    approach: BaseBranchResolutionApproach,
}
#[cfg(feature = "export-schema")]
but_schemars::register_sdk_type!(BaseBranchResolution);

#[derive(Serialize, Deserialize, PartialEq, Debug)]
#[cfg_attr(feature = "export-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct IntegrationOutcome {
    /// The list of branches that have been deleted as a result of the upstream integration
    deleted_branches: Vec<String>,
}
#[cfg(feature = "export-schema")]
but_schemars::register_sdk_type!(IntegrationOutcome);

impl StackStatus {
    fn create(
        tree_status: UpstreamTreeStatus,
        branch_statuses: Vec<NameAndStatus>,
    ) -> Result<Self> {
        if branch_statuses.is_empty() {
            bail!("Branch statuses must not be empty")
        }

        Ok(Self {
            tree_status,
            branch_statuses,
        })
    }

    fn resolution_acceptable(&self, approach: &ResolutionApproach) -> bool {
        if self.tree_status == UpstreamTreeStatus::Empty
            && self
                .branch_statuses
                .iter()
                .all(|branch_status| branch_status.status == BranchStatus::Integrated)
        {
            return matches!(
                approach,
                ResolutionApproach::Unapply | ResolutionApproach::Delete
            );
        }

        if self.is_single() {
            matches!(
                approach,
                ResolutionApproach::Merge
                    | ResolutionApproach::Rebase
                    | ResolutionApproach::Unapply
            )
        } else {
            matches!(
                approach,
                ResolutionApproach::Rebase | ResolutionApproach::Unapply
            )
        }
    }

    fn is_single(&self) -> bool {
        self.branch_statuses.len() == 1
    }
}

#[derive(Serialize, Deserialize, PartialEq, Debug)]
#[cfg_attr(feature = "export-schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Resolution {
    #[cfg_attr(
        feature = "export-schema",
        schemars(schema_with = "but_schemars::stack_id")
    )]
    pub stack_id: StackId,
    pub approach: ResolutionApproach,
    pub delete_integrated_branches: bool,
}
#[cfg(feature = "export-schema")]
but_schemars::register_sdk_type!(Resolution);

enum IntegrationResult {
    UpdatedObjects {
        head: gix::ObjectId,
        rebase_output: Option<RebaseOutput>,
        for_archival: Vec<Reference>,
    },
    UnapplyBranch,
    DeleteBranch,
}

pub struct UpstreamIntegrationContext<'a> {
    _permission: Option<&'a mut RepoExclusive>,
    ctx: &'a Context,
    stacks_in_workspace: Vec<but_workspace::legacy::ui::StackEntry>,
    new_target: gix::ObjectId,
    target_ref_name: gix::refs::FullName,
    old_target_id: gix::ObjectId,
    gix_repo: &'a gix::Repository,
    review_map: &'a HashMap<String, but_forge::ForgeReview>,
    upstream_commits: Vec<gix::ObjectId>,
}

impl<'a> UpstreamIntegrationContext<'a> {
    pub(crate) fn open(
        ctx: &'a Context,
        target_commit_oid: Option<gix::ObjectId>,
        permission: &'a mut RepoExclusive,
        gix_repo: &'a gix::Repository,
        review_map: &'a HashMap<String, but_forge::ForgeReview>,
    ) -> Result<Self> {
        {
            let meta = ctx.meta()?;
            let repo = ctx.repo.get()?;
            let _ref_info = but_workspace::head_info(
                &repo,
                &meta,
                Options {
                    expensive_commit_info: true,
                    traversal: but_graph::init::Options::limited(),
                },
            )?;
        }

        let (target_ref_name, old_target_id, upstream_commits) = {
            let (repo, ws, _db) = ctx.workspace_and_db_with_perm(permission.read_permission())?;
            let target_ref_name = ws
                .target_ref_name()
                .context("failed to get target reference name")?
                .to_owned();

            let upstream_commits = ws
                .upstream_commits(&repo, target_ref_name.as_ref(), FirstParent::Yes)?
                .into_iter()
                .map(|h| h.upstream_commits)
                .max_by_key(|us| us.len())
                .unwrap_or_default();
            (
                target_ref_name,
                ws.stored_target_commit_id()
                    .context("failed to get target base oid")?,
                upstream_commits,
            )
        };
        let new_target = match target_commit_oid {
            Some(oid) => oid,
            None => {
                gix_repo
                    .find_reference(target_ref_name.as_ref())?
                    .peel_to_commit()?
                    .id
            }
        };

        let stacks_in_workspace = stacks(ctx, gix_repo)?;

        Ok(Self {
            _permission: Some(permission),
            new_target,
            target_ref_name,
            old_target_id,
            stacks_in_workspace,
            ctx,
            gix_repo,
            review_map,
            upstream_commits,
        })
    }
}

#[expect(deprecated, reason = "calls but_workspace::legacy::stacks_v3")]
fn stacks(
    ctx: &Context,
    repo: &gix::Repository,
) -> anyhow::Result<Vec<but_workspace::legacy::ui::StackEntry>> {
    let meta = ctx.legacy_meta()?;
    but_workspace::legacy::stacks_v3(
        repo,
        &meta,
        but_workspace::legacy::StacksFilter::InWorkspace,
        None,
    )
}

/// Verify that all workspace stacks can be octopus-merged without conflicts.
///
/// When stacks have divergent upstream bases they can carry different versions
/// of the same file, causing the sequential octopus merge to fail. Instead of
/// silently evicting the offending stack, we return a descriptive error so the
/// user can unapply it manually and retry.
fn check_workspace_stacks_mergeable(
    context: &UpstreamIntegrationContext,
    repo: &gix::Repository,
) -> Result<()> {
    // Use an in-memory ODB so the intermediate merge trees don't pollute the
    // real object store — this is a read-only pre-flight check.
    let repo = repo.clone().with_object_memory();

    let target_tree = repo.find_commit(context.old_target_id)?.tree_id()?.detach();

    let (merge_options, conflict_kind) = repo.merge_options_fail_fast()?;

    let mut workspace_tree = target_tree;
    for stack in &context.stacks_in_workspace {
        let stack_tree = repo.find_commit(stack.tip)?.tree_id()?.detach();

        let mut merge = repo.merge_trees(
            target_tree,
            workspace_tree,
            stack_tree,
            repo.default_merge_labels(),
            merge_options.clone(),
        )?;

        if merge.has_unresolved_conflicts(conflict_kind) {
            let conflicting_files: Vec<String> = merge
                .conflicts
                .iter()
                .filter(|c| c.is_unresolved(TreatAsUnresolved::git()))
                .map(|c| c.ours.location().to_str_lossy().into_owned())
                .collect();

            let stack_name = stack
                .name()
                .map(|n| n.to_str_lossy().into_owned())
                .unwrap_or_else(|| "<unnamed>".to_string());

            bail!(
                "Stack '{stack_name}' conflicts with other applied stacks on: {}. \
                 Please unapply it and try again.",
                conflicting_files.join(", ")
            );
        }

        workspace_tree = merge.tree.write()?.detach();
    }

    Ok(())
}

#[expect(deprecated, reason = "calls but_workspace::legacy::stack_details_v3")]
fn stack_details(
    ctx: &Context,
    stack_id: Option<StackId>,
) -> anyhow::Result<but_workspace::ui::StackDetails> {
    let repo = ctx.clone_repo_for_merging_non_persisting()?;
    let meta = ctx.legacy_meta()?;
    but_workspace::legacy::stack_details_v3(stack_id, &repo, &meta)
}

/// Returns the status of a stack.
fn get_stack_status(
    gix_repo: &gix::Repository,
    new_target_commit_id: gix::ObjectId,
    stack_id: Option<StackId>,
    review_map: &HashMap<String, but_forge::ForgeReview>,
    ctx: &Context,
) -> Result<StackStatus> {
    let mut last_head = new_target_commit_id;

    let mut branch_statuses: Vec<NameAndStatus> = vec![];

    let details = stack_details(ctx, stack_id)?;

    let branches = details.branch_details;
    for branch in branches.into_iter().rev() {
        let local_commits = &branch.commits;

        let Some(branch_head) = local_commits.first() else {
            branch_statuses.push(NameAndStatus {
                name: branch.name.to_string(),
                status: BranchStatus::Empty,
            });

            continue;
        };

        let branch_head_string = branch_head.id.to_string();

        // Check if the branch has been integrated (either via review or commits)
        let is_integrated_via_review = review_map
            .get(&branch.name.to_string())
            .is_some_and(|review| review.is_merged_at_commit(&branch_head_string));
        let is_integrated_via_commits = matches!(
            branch_head.state,
            but_workspace::ui::CommitState::Integrated
        );

        if is_integrated_via_commits || is_integrated_via_review {
            branch_statuses.push(NameAndStatus {
                name: branch.name.to_string(),
                status: BranchStatus::Integrated,
            });

            continue;
        }
        // Rebase the commits and see if any conflict
        // Rebasing is preferable to merging, as not everything that is
        // mergeable is rebasable.
        // Doing both would be preferable, but we don't communicate that
        // to the frontend at the minute.
        let local_commit_ids = local_commits
            .iter()
            .filter(|c| !matches!(c.state, but_workspace::ui::CommitState::Integrated))
            .map(|commit| commit.id)
            .rev()
            .collect::<Vec<_>>();

        let rebase_base = last_head;

        let steps: Vec<RebaseStep> = local_commit_ids
            .iter()
            .map(|commit_id| RebaseStep::Pick {
                commit_id: *commit_id,
                new_message: None,
            })
            .collect();
        let mut rebase = but_rebase::Rebase::new(gix_repo, Some(rebase_base), None)?;
        rebase.rebase_noops(false);
        rebase.steps(steps)?;
        let output = rebase.rebase()?;
        let new_head_oid = output.top_commit;

        let any_conflicted = output.commit_mapping.iter().any(|(_base, _old, new)| {
            if let Ok(commit) = gix_repo.find_commit(*new) {
                commit.is_conflicted()
            } else {
                false
            }
        });

        last_head = new_head_oid;

        branch_statuses.push(NameAndStatus {
            name: branch.name.to_string(),
            status: if any_conflicted {
                BranchStatus::Conflicted { rebasable: false }
            } else {
                BranchStatus::SafelyUpdatable
            },
        });
    }

    StackStatus::create(UpstreamTreeStatus::Empty, branch_statuses)
}

pub fn upstream_integration_statuses(
    context: &UpstreamIntegrationContext,
) -> Result<StackStatuses> {
    let UpstreamIntegrationContext {
        new_target,
        stacks_in_workspace,
        review_map,
        ctx,
        upstream_commits,
        ..
    } = context;

    let repo = ctx.clone_repo_for_merging()?;
    let repo_in_memory = repo.clone().with_object_memory();

    if upstream_commits.is_empty() {
        return Ok(StackStatuses::UpToDate);
    };

    let heads = stacks_in_workspace
        .iter()
        .map(|stack| stack.tip)
        .chain(std::iter::once(*new_target))
        .collect::<Vec<_>>();

    // The merge base tree of all of the applied stacks plus the new target
    let merge_base_tree = repo
        .merge_base_octopus(heads)?
        .object()?
        .into_commit()
        .tree_id()?;

    // The working directory tree
    #[expect(deprecated, reason = "calls repo.create_wd_tree")]
    let workdir_tree = repo.create_wd_tree(gitbutler_project::AUTO_TRACK_LIMIT_BYTES)?;

    // The target tree
    let target_tree = repo.find_commit(*new_target)?.tree_id()?;

    let (merge_options_fail_fast, _conflict_kind) = repo.merge_options_no_rewrites_fail_fast()?;

    let merge_outcome = repo.merge_trees(
        merge_base_tree,
        repo.head()?.peel_to_commit()?.tree_id()?,
        target_tree,
        repo.default_merge_labels(),
        merge_options_fail_fast.clone(),
    )?;
    let committed_conflicts = merge_outcome
        .conflicts
        .iter()
        .filter(|c| c.is_unresolved(TreatAsUnresolved::git()))
        .collect::<Vec<_>>();

    let worktree_conflicts = repo
        .merge_trees(
            merge_base_tree,
            workdir_tree,
            target_tree,
            repo.default_merge_labels(),
            merge_options_fail_fast.clone(),
        )?
        .conflicts
        .iter()
        .filter(|c| c.is_unresolved(TreatAsUnresolved::git()))
        // only include conflicts that are not in the list committed_conflicts
        .filter(|c| !committed_conflicts.iter().any(|cc| cc.ours == c.ours))
        .map(|c| c.ours.location().into())
        .collect::<Vec<BStringForFrontend>>();

    let statuses = stacks_in_workspace
        .iter()
        .map(|stack| {
            Ok((
                stack.id,
                get_stack_status(
                    &repo_in_memory,
                    *new_target,
                    stack.id,
                    review_map,
                    context.ctx,
                )?,
            ))
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(StackStatuses::UpdatesRequired {
        worktree_conflicts,
        statuses,
    })
}

pub(crate) fn integrate_upstream(
    ctx: &Context,
    resolutions: &[Resolution],
    base_branch_resolution: Option<BaseBranchResolution>,
    review_map: &HashMap<String, but_forge::ForgeReview>,
    permission: &mut RepoExclusive,
) -> Result<IntegrationOutcome> {
    let old_workspace = WorkspaceState::create(ctx, permission.read_permission())?;

    let (target_commit_oid, base_branch_resolution_approach) = base_branch_resolution
        .map(|r| (Some(r.target_commit_oid), Some(r.approach)))
        .unwrap_or((None, None));

    let repo = ctx.repo.get()?;
    let context =
        UpstreamIntegrationContext::open(ctx, target_commit_oid, permission, &repo, review_map)?;

    // Check that all workspace stacks can be merged together. If any pair
    // of stacks carries conflicting trees (e.g. from divergent upstream
    // bases), ask the user to unapply the offending stack first.
    check_workspace_stacks_mergeable(&context, &repo)?;

    let mut virtual_branches_state = VirtualBranchesHandle::new(ctx.project_data_dir());

    let mut deleted_branches = vec![];

    // Ensure resolutions match current statuses
    {
        let statuses = upstream_integration_statuses(&context)?;

        let StackStatuses::UpdatesRequired { statuses, .. } = statuses else {
            return Ok(IntegrationOutcome {
                deleted_branches: vec![],
            });
        };

        if resolutions.len() != context.stacks_in_workspace.len() {
            bail!(
                "Chosen resolutions do not match quantity of applied virtual branches. {:?} {:?}",
                resolutions,
                context.stacks_in_workspace
            )
        }

        let all_resolutions_are_up_to_date = resolutions.iter().all(|resolution| {
            let Some(status) = statuses
                .iter()
                .find(|status| status.0 == Some(resolution.stack_id))
            else {
                return false;
            };

            status.1.resolution_acceptable(&resolution.approach)
        });

        if !all_resolutions_are_up_to_date {
            bail!("Chosen resolutions do not match current integration statuses")
        }
    }

    let integration_results =
        compute_resolutions(&context, resolutions, base_branch_resolution_approach)?;

    {
        // We perform the updates in stages. If deleting or unapplying fails, we
        // could enter a much worse state if we're simultaneously updating trees

        // Delete branches
        for (maybe_stack_id, integration_result) in &integration_results {
            if !matches!(integration_result, IntegrationResult::DeleteBranch) {
                continue;
            };

            let Some(stack_id) = maybe_stack_id else {
                // If the stack ID is not defined, we're on single-branch mode, so nothing to delete.
                continue;
            };

            let maybe_stack = context
                .stacks_in_workspace
                .iter()
                .find(|s| s.id == Some(*stack_id));

            let Some(stack) = maybe_stack else {
                // The integration results should match the stacks in the workspace,
                // so this should never happen.
                bail!("Failed to find stack while integrating upstream: {stack_id:?}");
            };

            virtual_branches_state.delete_branch_entry(stack_id)?;
            let delete_local_refs = resolutions
                .iter()
                .find(|r| r.stack_id == *stack_id)
                .map(|r| r.delete_integrated_branches)
                .unwrap_or(false);

            if delete_local_refs {
                for head in &stack.heads {
                    let branch_name = head.name.to_str().context("Invalid branch name")?;
                    match head.delete_reference(&repo) {
                        Ok(_) => {
                            deleted_branches.push(branch_name.to_string());
                        }
                        _ => {
                            // Fail silently because interrupting this is worse
                        }
                    }
                }
            }
        }

        let permission = context._permission.expect("Permission provided above");

        // Unapply branches
        for (maybe_stack_id, integration_result) in &integration_results {
            if !matches!(integration_result, IntegrationResult::UnapplyBranch) {
                continue;
            };

            let Some(stack_id) = maybe_stack_id else {
                // If the stack ID is not defined, we're on single-branch mode, so nothing to unapply.
                continue;
            };

            ctx.branch_manager().unapply(
                *stack_id,
                permission,
                false,
                Vec::new(),
                ctx.settings.feature_flags.cv3,
            )?;
        }

        let mut stacks = virtual_branches_state.list_stacks_in_workspace()?;

        {
            let workspace_ref: gix::refs::FullName = WORKSPACE_REF_NAME.try_into()?;
            let mut meta = ctx.legacy_meta()?;
            let mut workspace = meta.workspace(workspace_ref.as_ref())?;
            workspace.target_commit_id = Some(context.new_target);
            meta.set_workspace(&workspace)?;
            meta.write_unreconciled()?;
            ctx.invalidate_workspace_cache()?;
        }

        // Update branch trees
        for (maybe_stack_id, integration_result) in &integration_results {
            let IntegrationResult::UpdatedObjects {
                head,
                rebase_output,
                for_archival,
            } = integration_result
            else {
                continue;
            };

            let Some(stack_id) = maybe_stack_id else {
                // If the stack ID is not defined, we're on single-branch mode and there's nothing to update.
                continue;
            };

            let Some(stack) = stacks.iter_mut().find(|stack| stack.id == *stack_id) else {
                continue;
            };

            let delete_local_refs = resolutions
                .iter()
                .find(|r| r.stack_id == *stack_id)
                .map(|r| r.delete_integrated_branches)
                .unwrap_or(false);

            // Archive integrated heads before updating branch heads.
            // `for_archival` captures branches that lost all commits during
            // integrated-commit filtering. We must archive these before
            // calling `set_heads_from_rebase_output`, which validates that
            // rebase references match exactly the non-archived heads.
            let stack_branches_deleted =
                stack.archive_integrated_heads(ctx, &repo, for_archival, delete_local_refs)?;
            deleted_branches.extend(stack_branches_deleted);

            // Update the branch heads, filtering out references for archived
            // heads so the validation in `set_all_heads` sees a consistent set.
            if let Some(output) = rebase_output {
                let archived_names: HashSet<&str> = stack
                    .heads
                    .iter()
                    .filter(|h| h.archived)
                    .map(|h| h.name().as_str())
                    .collect();
                let active_references: Vec<_> = output
                    .references
                    .iter()
                    .filter(|r| !archived_names.contains(r.reference.to_string().as_str()))
                    .cloned()
                    .collect();
                stack.set_heads_from_rebase_output(ctx, active_references)?;
            }

            // Dissociate closed reviews
            for head in stack.clone().heads.iter() {
                let branch_name = head.name.to_string();
                if let Some(review) = review_map.get(&branch_name)
                    && !review.is_open()
                {
                    stack.set_pr_number(ctx, &branch_name, None)?;
                }
            }

            stack.set_stack_head(&mut virtual_branches_state, &repo, *head)?;
        }

        {
            let new_workspace = WorkspaceState::create(ctx, permission.read_permission())?;
            update_uncommitted_changes(ctx, old_workspace, new_workspace, permission)?;
        }

        crate::integration::update_workspace_commit_with_vb_state(
            &virtual_branches_state,
            ctx,
            false,
        )?;
    }

    deleted_branches.sort();
    deleted_branches.dedup();

    Ok(IntegrationOutcome { deleted_branches })
}

pub fn worktree_conflict_preview(
    context: &UpstreamIntegrationContext,
    path: &str,
) -> Result<Option<WorktreeConflictPreview>> {
    let repo = context.ctx.repo.get()?;
    let stacks_in_workspace = &context.stacks_in_workspace;
    let heads = stacks_in_workspace
        .iter()
        .map(|stack| stack.tip)
        .chain(std::iter::once(context.new_target))
        .collect::<Vec<_>>();
    let merge_base_tree = repo
        .merge_base_octopus(heads)?
        .object()?
        .into_commit()
        .tree_id()?;
    #[expect(deprecated, reason = "calls repo.create_wd_tree")]
    let workdir_tree = repo.create_wd_tree(gitbutler_project::AUTO_TRACK_LIMIT_BYTES)?;
    let target_tree = repo.find_commit(context.new_target)?.tree_id()?;
    let (merge_options_fail_fast, _conflict_kind) = repo.merge_options_no_rewrites_fail_fast()?;
    let merge = repo.merge_trees(
        merge_base_tree,
        workdir_tree,
        target_tree,
        repo.default_merge_labels(),
        merge_options_fail_fast,
    )?;

    let normalized_path = path.replace('\\', "/");
    let Some(conflict) = merge
        .conflicts
        .iter()
        .find(|conflict| conflict.ours.location().to_str_lossy().as_ref() == normalized_path)
    else {
        return Ok(None);
    };

    let session = UnityConflictSession::create(context.ctx.project_data_dir(), &normalized_path)?;
    let [base_entry, local_entry, upstream_entry] = conflict.entries();
    let base = materialize_preview_side(
        &repo,
        &session,
        &normalized_path,
        PreviewSide::Base,
        base_entry.map(|entry| entry.id),
    )?;
    let local = materialize_preview_side(
        &repo,
        &session,
        &normalized_path,
        PreviewSide::Local,
        local_entry.map(|entry| entry.id),
    )?;
    let upstream = materialize_preview_side(
        &repo,
        &session,
        &normalized_path,
        PreviewSide::Upstream,
        upstream_entry.map(|entry| entry.id),
    )?;

    let lfs = UnityConflictLfsInfo {
        tracked: base.lfs_tracked || local.lfs_tracked || upstream.lfs_tracked,
        base: base.side_state(),
        local: local.side_state(),
        upstream: upstream.side_state(),
    };
    let available_choices = [
        (&local, UnityConflictSide::Local),
        (&upstream, UnityConflictSide::Upstream),
    ]
    .into_iter()
    .filter_map(|(side, choice)| side.text_ready().then_some(choice))
    .collect::<Vec<_>>();

    let mut session_meta = UnityConflictSessionMeta {
        path: normalized_path.clone(),
        base: base.file.clone(),
        local: local.file.clone(),
        upstream: upstream.file.clone(),
        merged: None,
    };

    if let (Some(base_file), Some(local_file), Some(upstream_file)) =
        (&base.file, &local.file, &upstream.file)
        && base.text_ready()
        && local.text_ready()
        && upstream.text_ready()
    {
        let merged = session.file_path("merged");
        match merge_preview_files(base_file, local_file, upstream_file, &merged)
            .and_then(|()| parse_unity_conflict_blocks(&merged))
        {
            Ok(blocks) if !blocks.is_empty() => {
                session_meta.merged = Some(merged);
                session.write_meta(&session_meta)?;
                return Ok(Some(WorktreeConflictPreview {
                    path: path.to_owned(),
                    mode: WorktreeConflictPreviewMode::MergePreview,
                    session_id: session.id,
                    lfs,
                    document: Some(UnityConflictPreviewDocument {
                        path: path.to_owned(),
                        blocks,
                    }),
                    available_choices,
                    message: None,
                }));
            }
            Ok(_) => {
                session_meta.merged = Some(merged);
            }
            Err(err) => {
                tracing::warn!(
                    ?err,
                    path = normalized_path,
                    "failed to build Unity merge preview"
                );
            }
        }
    }

    session.write_meta(&session_meta)?;
    Ok(Some(WorktreeConflictPreview {
        path: path.to_owned(),
        mode: WorktreeConflictPreviewMode::ChooseSide,
        session_id: session.id,
        lfs,
        document: None,
        available_choices,
        message: Some(
            "GitButler could not build a pointer-safe scene merge preview. Choose the local or upstream file explicitly."
                .to_owned(),
        ),
    }))
}

#[derive(Clone, Copy)]
enum PreviewSide {
    Base,
    Local,
    Upstream,
}

struct MaterializedPreviewSide {
    file: Option<PathBuf>,
    state: UnityConflictMaterializeState,
    size: Option<u64>,
    lfs_tracked: bool,
}

impl MaterializedPreviewSide {
    fn text_ready(&self) -> bool {
        self.state == UnityConflictMaterializeState::TextReady
    }

    fn side_state(&self) -> UnityConflictSideState {
        UnityConflictSideState {
            state: self.state,
            size: self.size,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct UnityConflictSessionMeta {
    path: String,
    base: Option<PathBuf>,
    local: Option<PathBuf>,
    upstream: Option<PathBuf>,
    merged: Option<PathBuf>,
}

struct UnityConflictSession {
    id: String,
    dir: PathBuf,
}

impl UnityConflictSession {
    fn create(project_data_dir: PathBuf, path: &str) -> Result<Self> {
        let id = Uuid::new_v4().to_string();
        let dir = project_data_dir.join("unity-conflict-sessions").join(&id);
        fs::create_dir_all(&dir)
            .with_context(|| format!("failed to create Unity conflict session for {path}"))?;
        Ok(Self { id, dir })
    }

    fn open(project_data_dir: PathBuf, id: &str) -> Result<Self> {
        validate_session_id(id)?;
        let dir = project_data_dir.join("unity-conflict-sessions").join(id);
        if !dir.is_dir() {
            bail!("Unity conflict session is no longer available");
        }
        Ok(Self {
            id: id.to_owned(),
            dir,
        })
    }

    fn file_path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    fn write_meta(&self, meta: &UnityConflictSessionMeta) -> Result<()> {
        fs::write(
            self.dir.join("session.json"),
            serde_json::to_vec_pretty(meta)?,
        )
        .context("failed to write Unity conflict session metadata")
    }

    fn read_meta(&self) -> Result<UnityConflictSessionMeta> {
        Ok(serde_json::from_slice(&fs::read(
            self.dir.join("session.json"),
        )?)?)
    }
}

fn validate_session_id(id: &str) -> Result<()> {
    if id
        .bytes()
        .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
    {
        Ok(())
    } else {
        bail!("Invalid Unity conflict session id")
    }
}

fn materialize_preview_side(
    repo: &gix::Repository,
    session: &UnityConflictSession,
    path: &str,
    side: PreviewSide,
    entry_id: Option<gix::ObjectId>,
) -> Result<MaterializedPreviewSide> {
    let Some(entry_id) = entry_id else {
        return Ok(MaterializedPreviewSide {
            file: None,
            state: UnityConflictMaterializeState::Absent,
            size: None,
            lfs_tracked: false,
        });
    };
    let object = repo.find_object(entry_id)?;
    let blob = object.into_blob();
    let data = &blob.data;
    let destination = session.file_path(match side {
        PreviewSide::Base => "base",
        PreviewSide::Local => "local",
        PreviewSide::Upstream => "upstream",
    });

    let Some(pointer) = but_core::lfs::parse_lfs_pointer(data) else {
        fs::write(&destination, data)?;
        return Ok(classify_materialized_file(destination, None, false));
    };

    if matches!(side, PreviewSide::Local)
        && let Some(workdir) = repo.workdir()
    {
        let worktree_path = workdir.join(path);
        if worktree_path.is_file() {
            copy_file(&worktree_path, &destination)?;
            if !file_starts_with_lfs_pointer(&destination)? {
                return Ok(classify_materialized_file(
                    destination,
                    Some(pointer.size),
                    true,
                ));
            }
        }
    }

    match smudge_lfs_pointer_to_file(repo, path, data, &destination) {
        Ok(()) => Ok(classify_materialized_file(
            destination,
            Some(pointer.size),
            true,
        )),
        Err(err) => {
            tracing::warn!(
                ?err,
                path,
                "failed to materialize Git LFS pointer for Unity preview"
            );
            Ok(MaterializedPreviewSide {
                file: None,
                state: UnityConflictMaterializeState::MissingLfsObject,
                size: Some(pointer.size),
                lfs_tracked: true,
            })
        }
    }
}

fn classify_materialized_file(
    path: PathBuf,
    size: Option<u64>,
    lfs_tracked: bool,
) -> MaterializedPreviewSide {
    let state = match file_starts_with_lfs_pointer(&path) {
        Ok(true) => UnityConflictMaterializeState::PointerStillPresent,
        Ok(false) => UnityConflictMaterializeState::TextReady,
        Err(_) => UnityConflictMaterializeState::BinaryOrNonUtf8,
    };
    MaterializedPreviewSide {
        file: Some(path),
        state,
        size,
        lfs_tracked,
    }
}

fn copy_file(from: &Path, to: &Path) -> Result<()> {
    let mut input =
        File::open(from).with_context(|| format!("failed to open {}", from.display()))?;
    let mut output =
        File::create(to).with_context(|| format!("failed to create {}", to.display()))?;
    io::copy(&mut input, &mut output)?;
    Ok(())
}

fn file_starts_with_lfs_pointer(path: &Path) -> Result<bool> {
    let mut file = File::open(path)?;
    let mut buffer = [0_u8; 256];
    let bytes_read = file.read(&mut buffer)?;
    Ok(but_core::lfs::is_lfs_pointer(&buffer[..bytes_read]))
}

fn smudge_lfs_pointer_to_file(
    repo: &gix::Repository,
    path: &str,
    pointer: &[u8],
    destination: &Path,
) -> Result<()> {
    let workdir = repo
        .workdir()
        .context("Git LFS Unity conflict previews require a worktree")?;
    let mut child = Command::new("git")
        .arg("lfs")
        .arg("smudge")
        .arg("--")
        .arg(path)
        .current_dir(workdir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("Failed to start git lfs smudge for Unity conflict preview")?;

    {
        let stdin = child
            .stdin
            .as_mut()
            .context("Failed to open git lfs smudge stdin")?;
        stdin
            .write_all(pointer)
            .context("Failed to send Git LFS pointer to smudge")?;
    }

    let mut stdout = child
        .stdout
        .take()
        .context("Failed to read Git LFS smudge output")?;
    let mut output_file = File::create(destination)?;
    io::copy(&mut stdout, &mut output_file)?;
    let output = child
        .wait_with_output()
        .context("Failed to finish git lfs smudge for Unity conflict preview")?;
    if !output.status.success() {
        bail!(
            "Git LFS could not load {} for Unity conflict preview: {}",
            path,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    if file_starts_with_lfs_pointer(destination)? {
        bail!(
            "Git LFS returned another pointer for {}; run `git lfs pull --include=\"{}\"` and try again.",
            path,
            path
        );
    }
    Ok(())
}

fn merge_preview_files(base: &Path, local: &Path, upstream: &Path, merged: &Path) -> Result<()> {
    let output = Command::new("git")
        .arg("merge-file")
        .arg("-p")
        .arg("-L")
        .arg("local")
        .arg("-L")
        .arg("base")
        .arg("-L")
        .arg("upstream")
        .arg(local)
        .arg(base)
        .arg(upstream)
        .output()
        .context("failed to run git merge-file for Unity conflict preview")?;
    if !output.status.success() && output.status.code() != Some(1) {
        bail!(
            "git merge-file could not build a Unity conflict preview: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    fs::write(merged, output.stdout)?;
    Ok(())
}

fn parse_unity_conflict_blocks(path: &Path) -> Result<Vec<UnityConflictPreviewBlock>> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut blocks = Vec::new();
    let mut context = String::new();
    let mut lines = reader.lines();

    while let Some(line) = lines.next() {
        let line = line.context("Unity conflict preview is not valid UTF-8")?;
        if !line.starts_with("<<<<<<<") {
            remember_context_line(&mut context, &line);
            continue;
        }

        let mut ours = String::new();
        for line in lines.by_ref() {
            let line = line.context("Unity conflict preview is not valid UTF-8")?;
            if line.starts_with("=======") {
                break;
            }
            ours.push_str(&line);
            ours.push('\n');
        }

        let mut theirs = String::new();
        for line in lines.by_ref() {
            let line = line.context("Unity conflict preview is not valid UTF-8")?;
            if line.starts_with(">>>>>>>") {
                break;
            }
            theirs.push_str(&line);
            theirs.push('\n');
        }

        let index = blocks.len() + 1;
        blocks.push(UnityConflictPreviewBlock {
            id: format!("conflict-{index}"),
            label: infer_unity_conflict_label(&context, &ours, &theirs, index),
            context: if context.is_empty() {
                "Unity YAML conflict".to_owned()
            } else {
                context.clone()
            },
            ours,
            theirs,
            fields: Vec::new(),
        });
    }

    Ok(blocks)
}

fn remember_context_line(context: &mut String, line: &str) {
    let trimmed = line.trim();
    if trimmed.is_empty()
        || trimmed.starts_with("%YAML")
        || trimmed.starts_with("%TAG")
        || trimmed.starts_with("--- !u!")
    {
        return;
    }
    *context = trimmed.chars().take(120).collect();
}

fn infer_unity_conflict_label(context: &str, ours: &str, theirs: &str, index: usize) -> String {
    ours.lines()
        .chain(theirs.lines())
        .map(str::trim)
        .find(|line| {
            line.split_once(':')
                .is_some_and(|(key, _)| key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
        })
        .and_then(|line| line.split_once(':').map(|(key, _)| key.to_owned()))
        .or_else(|| (!context.is_empty()).then(|| context.to_owned()))
        .unwrap_or_else(|| format!("Conflict {index}"))
}

pub fn apply_unity_conflict_resolution(
    ctx: &Context,
    input: UnityConflictResolutionInput,
) -> Result<()> {
    let session = UnityConflictSession::open(ctx.project_data_dir(), &input.session_id)?;
    let meta = session.read_meta()?;
    if meta.path != input.path.replace('\\', "/") {
        bail!("Unity conflict resolution path does not match the preview session");
    }

    let workdir = ctx.workdir_or_fail()?;
    let relative = safe_relative_path(&meta.path)?;
    let destination = workdir.join(&relative);
    snapshot_unity_file(ctx, &destination, &meta.path)?;

    let resolved = session.file_path("resolved");
    match input.resolution {
        UnityConflictResolution::Blocks { blocks } => {
            let Some(merged) = meta.merged else {
                bail!("Unity conflict session does not have a merge preview to resolve");
            };
            apply_block_resolutions(&merged, &resolved, &blocks)?;
        }
        UnityConflictResolution::Local => {
            let Some(local) = meta.local else {
                bail!("Local Unity file is not available for this conflict");
            };
            copy_file(&local, &resolved)?;
        }
        UnityConflictResolution::Upstream => {
            let Some(upstream) = meta.upstream else {
                bail!(
                    "Upstream Unity file is not available for this conflict. Run `git lfs pull` and try again."
                );
            };
            copy_file(&upstream, &resolved)?;
        }
    }

    validate_resolved_unity_file(&resolved)?;
    replace_worktree_file(&resolved, &destination)?;
    validate_resolved_unity_file(&destination)?;
    Ok(())
}

fn apply_block_resolutions(
    merged: &Path,
    resolved: &Path,
    blocks: &HashMap<String, String>,
) -> Result<()> {
    let input = File::open(merged)?;
    let mut output = File::create(resolved)?;
    let reader = BufReader::new(input);
    let mut lines = reader.lines();
    let mut block_index = 0;

    while let Some(line) = lines.next() {
        let line = line.context("Unity conflict preview is not valid UTF-8")?;
        if !line.starts_with("<<<<<<<") {
            writeln!(output, "{line}")?;
            continue;
        }

        block_index += 1;
        let block_id = format!("conflict-{block_index}");
        for line in lines.by_ref() {
            let line = line.context("Unity conflict preview is not valid UTF-8")?;
            if line.starts_with("=======") {
                break;
            }
        }
        for line in lines.by_ref() {
            let line = line.context("Unity conflict preview is not valid UTF-8")?;
            if line.starts_with(">>>>>>>") {
                break;
            }
        }

        let replacement = blocks
            .get(&block_id)
            .with_context(|| format!("Missing Unity conflict resolution for {block_id}"))?;
        output.write_all(replacement.as_bytes())?;
        if !replacement.ends_with('\n') {
            writeln!(output)?;
        }
    }

    Ok(())
}

fn validate_resolved_unity_file(path: &Path) -> Result<()> {
    if file_starts_with_lfs_pointer(path)? {
        bail!(
            "Refusing to write Git LFS pointer text as a Unity scene. Run `git lfs pull` and try again."
        );
    }
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    for line in reader.lines() {
        let line = line.context("Resolved Unity file is not valid UTF-8")?;
        if line.starts_with("<<<<<<<") || line.starts_with("=======") || line.starts_with(">>>>>>>")
        {
            bail!("The resolved Unity scene still contains conflict markers");
        }
    }
    Ok(())
}

fn replace_worktree_file(source: &Path, destination: &Path) -> Result<()> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    let parent = destination
        .parent()
        .context("Unity conflict destination must have a parent directory")?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    {
        let mut input = File::open(source)?;
        io::copy(&mut input, &mut temp)?;
        temp.flush()?;
    }
    temp.persist(destination)
        .map(|_| ())
        .map_err(|err| err.error)
        .context("failed to replace resolved Unity file")
}

fn snapshot_unity_file(ctx: &Context, destination: &Path, display_path: &str) -> Result<()> {
    if !destination.exists() {
        return Ok(());
    }
    let snapshot_root = ctx
        .project_data_dir()
        .join("unity-conflict-snapshots")
        .join(format!(
            "{}-{}",
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
            Uuid::new_v4()
        ));
    let relative = safe_relative_path(display_path)?;
    let snapshot_path = snapshot_root.join(&relative);
    if let Some(parent) = snapshot_path.parent() {
        fs::create_dir_all(parent)?;
    }
    copy_file(destination, &snapshot_path)?;
    let pointer = {
        let mut bytes = Vec::new();
        File::open(destination)?.take(512).read_to_end(&mut bytes)?;
        but_core::lfs::parse_lfs_pointer(&bytes)
    };
    fs::write(
        snapshot_root.join("metadata.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "path": display_path,
            "snapshotPath": snapshot_path,
            "lfsPointer": pointer,
        }))?,
    )?;
    Ok(())
}

fn safe_relative_path(path: &str) -> Result<PathBuf> {
    let path = Path::new(path);
    if path.is_absolute() {
        bail!("Unity conflict path must be relative");
    }
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => out.push(value),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                bail!("Unity conflict path escapes the repository")
            }
        }
    }
    if out.as_os_str().is_empty() {
        bail!("Unity conflict path is empty");
    }
    Ok(out)
}

pub(crate) fn resolve_upstream_integration(
    ctx: &Context,
    resolution_approach: BaseBranchResolutionApproach,
    review_map: &HashMap<String, but_forge::ForgeReview>,
    permission: &mut RepoExclusive,
) -> Result<gix::ObjectId> {
    let repo = ctx.repo.get()?;
    let context = UpstreamIntegrationContext::open(ctx, None, permission, &repo, review_map)?;
    let new_target_id = context.new_target;
    let old_target_id = context.old_target_id;
    let fork_point = repo.merge_base(old_target_id, new_target_id)?.detach();

    match resolution_approach {
        BaseBranchResolutionApproach::HardReset => Ok(new_target_id),
        BaseBranchResolutionApproach::Merge => {
            let branch_name = context.target_ref_name.as_bstr().to_str_lossy();
            let new_head = merge_commits(
                &repo,
                old_target_id,
                context.new_target,
                &format!("Merge `{branch_name}` into `{branch_name}`"),
            )?;

            Ok(new_head)
        }
        BaseBranchResolutionApproach::Rebase => {
            let steps = first_parent_commit_ids_until(&repo, old_target_id, fork_point)?
                .into_iter()
                .map(|commit_id| RebaseStep::Pick {
                    commit_id,
                    new_message: None,
                })
                .collect::<Vec<_>>();
            let mut rebase = but_rebase::Rebase::new(&repo, Some(new_target_id), None)?;
            rebase.steps(steps)?;
            rebase.rebase_noops(false);
            let outcome = rebase.rebase()?;
            Ok(outcome.top_commit)
        }
    }
}

fn compute_resolutions(
    context: &UpstreamIntegrationContext,
    resolutions: &[Resolution],
    base_branch_resolution_approach: Option<BaseBranchResolutionApproach>,
) -> Result<Vec<(Option<StackId>, IntegrationResult)>> {
    let UpstreamIntegrationContext {
        new_target,
        target_ref_name,
        old_target_id,
        stacks_in_workspace,
        gix_repo,
        ..
    } = context;

    let results = resolutions
        .iter()
        .map(|resolution| {
            let Some(stack) = stacks_in_workspace
                .iter()
                .find(|stack| stack.id == Some(resolution.stack_id))
            else {
                bail!("Failed to find virtual branch");
            };

            match resolution.approach {
                ResolutionApproach::Unapply => Ok((stack.id, IntegrationResult::UnapplyBranch)),
                ResolutionApproach::Delete => Ok((stack.id, IntegrationResult::DeleteBranch)),
                ResolutionApproach::Merge => {
                    // Make a merge commit. It will be set as a stack head later.
                    let top_branch = stack.heads.last().context("top branch not found")?;

                    // These two go into the merge commit message.
                    let incoming_branch_name = target_ref_name.as_bstr().to_str_lossy();
                    let target_branch_name = top_branch.name.to_str()?;

                    let new_head = merge_commits(
                        gix_repo,
                        stack.tip,
                        *new_target,
                        &format!("Merge `{incoming_branch_name}` into `{target_branch_name}`"),
                    )?;

                    Ok((
                        stack.id,
                        IntegrationResult::UpdatedObjects {
                            head: new_head,
                            rebase_output: None,
                            for_archival: vec![],
                        },
                    ))
                }
                ResolutionApproach::Rebase => {
                    // Rebase the commits, then try rebasing the tree. If
                    // the tree ends up conflicted, commit the tree.

                    // If the base branch needs to resolve its divergence
                    // pick only the commits that are ahead of the old target head
                    let lower_bound = if base_branch_resolution_approach.is_some() {
                        *old_target_id
                    } else {
                        *new_target
                    };

                    let details = stack_details(context.ctx, stack.id)?;
                    let mut commit_map = HashMap::new();
                    for branch in &details.branch_details {
                        for commit in &branch.commits {
                            commit_map.insert(commit.id, commit.clone());
                        }
                    }

                    let all_steps = details.as_rebase_steps(context.gix_repo)?;
                    let branches_before = as_buckets(all_steps.clone());
                    // Filter out any integrated commits
                    let steps = all_steps
                        .into_iter()
                        .filter_map(|s| match s {
                            RebaseStep::Pick {
                                commit_id,
                                new_message: _,
                            } => {
                                let is_integrated = commit_map.get(&commit_id).is_some_and(|c| {
                                    matches!(c.state, but_workspace::ui::CommitState::Integrated)
                                });
                                if is_integrated { None } else { Some(s) }
                            }
                            _ => Some(s),
                        })
                        .collect::<Vec<_>>();

                    let branches_after = as_buckets(steps.clone());

                    // Branches that used to have commits but now don't are marked for archival
                    let mut for_archival = vec![];
                    for (ref_before, steps_before) in branches_before {
                        if let Some((_, steps_after)) = branches_after
                            .iter()
                            .find(|(ref_after, _)| ref_after == &ref_before)
                        {
                            // if there were steps before and now there are none, this should be marked for archival
                            if !steps_before.is_empty() && steps_after.is_empty() {
                                for_archival.push(ref_before);
                            }
                        }
                    }

                    let mut rebase =
                        but_rebase::Rebase::new(context.gix_repo, Some(lower_bound), None)?;
                    rebase.rebase_noops(false);
                    rebase.steps(steps)?;
                    let output = rebase.rebase()?;
                    let new_head = output.top_commit;

                    Ok((
                        stack.id,
                        IntegrationResult::UpdatedObjects {
                            head: new_head,
                            rebase_output: Some(output),
                            for_archival,
                        },
                    ))
                }
            }
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(results)
}

pub(crate) fn as_buckets(steps: Vec<RebaseStep>) -> Vec<(but_core::Reference, Vec<RebaseStep>)> {
    let mut buckets = vec![];
    let mut current_steps = vec![];
    for step in steps {
        match step {
            RebaseStep::Reference(reference) => {
                buckets.push((reference, std::mem::take(&mut current_steps)));
            }
            step => {
                current_steps.push(step);
            }
        }
    }
    buckets
}
