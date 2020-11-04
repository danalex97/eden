/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use std::fs::File;
use std::io::Read;

use anyhow::{anyhow, format_err, Error};
use clap::{App, ArgMatches, SubCommand};
use cmdlib::args;
use context::CoreContext;
use fbinit::FacebookInit;
use futures::{compat::Future01CompatExt, TryFutureExt};
use mononoke_types::{
    BonsaiChangeset, BonsaiChangesetMut, ChangesetId, DateTime, FileChange, MPath,
};
use serde_derive::Deserialize;
use slog::Logger;
use std::collections::BTreeMap;

use crate::error::SubcommandError;
use blobrepo::save_bonsai_changesets;
use blobrepo_hg::BlobRepoHg;

pub const CREATE_BONSAI: &str = "create-bonsai";

pub fn build_subcommand<'a, 'b>() -> App<'a, 'b> {
    SubCommand::with_name(CREATE_BONSAI)
        .about("Create and push bonsai changeset")
        .args_from_usage(
            r#"<BONSAI_FILE> 'path to json of changes'
            --dangerous 'It's dangerous command. Do you really need to run this command?'"#,
        )
}

pub async fn subcommand_create_bonsai<'a>(
    fb: FacebookInit,
    logger: Logger,
    matches: &'a ArgMatches<'_>,
    sub_m: &'a ArgMatches<'_>,
) -> Result<(), SubcommandError> {
    if !sub_m.is_present("dangerous") {
        return Err(SubcommandError::Error(anyhow!(
            "--dangerous was not provided. Think twice before use"
        )));
    }
    let path = sub_m.value_of("BONSAI_FILE").unwrap().to_string();

    let mut content = String::new();
    File::open(path)
        .map_err(|e| SubcommandError::Error(anyhow!(e)))?
        .read_to_string(&mut content)
        .map_err(|e| SubcommandError::Error(anyhow!(e)))?;

    args::init_cachelib(fb, &matches, None);

    let ctx = CoreContext::new_with_logger(fb, logger.clone());

    let bcs: BonsaiChangeset = match serde_json::from_str(&content) {
        Ok(val) => {
            let bcs_deser: DeserializableBonsaiChangeset = val;
            bcs_deser.into_bonsai()?.freeze()?
        }
        Err(e) => return Err(SubcommandError::Error(anyhow!(e))),
    };


    let blobrepo = args::open_repo(fb, &logger, &matches).await?;
    for (_, change) in bcs.file_changes() {
        if let Some(change) = change {
            if filestore::get_metadata(
                &blobrepo.get_blobstore(),
                ctx.clone(),
                &change.content_id().into(),
            )
            .compat()
            .await?
            .is_none()
            {
                return Err(SubcommandError::Error(format_err!(
                    "filenode {} is not found in the filestore",
                    &change.content_id()
                )));
            }
        }
    }
    let bcs_id = bcs.get_changeset_id();
    save_bonsai_changesets(vec![bcs], ctx.clone(), blobrepo.clone())
        .compat()
        .map_err(|e| SubcommandError::Error(anyhow!(e)))
        .await?;
    let hg_cs = blobrepo
        .get_hg_from_bonsai_changeset(ctx, bcs_id)
        .compat()
        .await?;
    println!(
        "Created bonsai changeset {} for hg_changeset {}",
        bcs_id, hg_cs
    );
    Ok(())
}

#[derive(Debug, Deserialize)]
pub struct DeserializableBonsaiChangeset {
    pub parents: Vec<ChangesetId>,
    pub author: String,
    pub author_date: DateTime,
    pub committer: Option<String>,
    // XXX should committer date always be recorded? If so, it should probably be a
    // monotonically increasing value:
    // max(author date, max(committer date of parents) + epsilon)
    pub committer_date: Option<DateTime>,
    pub message: String,
    pub extra: BTreeMap<String, Vec<u8>>,
    pub file_changes: BTreeMap<String, Option<FileChange>>,
}

impl DeserializableBonsaiChangeset {
    pub fn into_bonsai(self) -> Result<BonsaiChangesetMut, Error> {
        let files = self
            .file_changes
            .into_iter()
            .map::<Result<_, Error>, _>(|(path, changes)| {
                Ok((MPath::new(path.as_bytes())?, changes))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(BonsaiChangesetMut {
            parents: self.parents,
            author: self.author,
            author_date: self.author_date,
            committer: self.committer,
            committer_date: self.committer_date,
            message: self.message,
            extra: self.extra,
            file_changes: files.into_iter().collect(),
        })
    }
}