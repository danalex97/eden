/*
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use anyhow::Error;
use futures::{future::ok, Future};

use crate::core::CoreContext;

impl CoreContext {
    pub fn trace_upload(&self) -> impl Future<Item = (), Error = Error> {
        ok(())
    }
}
