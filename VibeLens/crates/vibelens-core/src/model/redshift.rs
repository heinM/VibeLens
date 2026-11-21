//! Amazon Redshift connection and catalog types (through the Redshift Data API). Mirrored in
//! `src/ipc/types.ts`.
//!
//! A Redshift profile holds no secret: every call is SigV4-signed with the AWS identity in
//! [`RedshiftOptions::aws`], and [`RedshiftAuth::Secret`] carries only the ARN of a Secrets
//! Manager secret that the Data API reads on the server side.

use serde::{Deserialize, Serialize};

use super::profile::AwsTarget;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RedshiftOptions {
    pub target: RedshiftTarget,
    /// Database statements run in unless a call names another one.
    pub database: String,
    /// Identity that signs every Data API call, and the region of the cluster or workgroup.
    pub aws: AwsTarget,
    /// Required in the file: a missing or unknown kind is rejected rather than defaulted (R9).
    pub auth: RedshiftAuth,
}

/// What the Data API talks to.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum RedshiftTarget {
    /// A provisioned cluster, by its cluster identifier.
    Provisioned { cluster_identifier: String },
    /// A Redshift Serverless workgroup, by name.
    Serverless { workgroup_name: String },
}

/// Who the statements run as inside the database.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum RedshiftAuth {
    /// The caller's IAM identity is the database user (`IAM:user` or `IAMR:role`). Needs
    /// `redshift:GetClusterCredentialsWithIAM` (provisioned) or
    /// `redshift-serverless:GetCredentials` (serverless).
    #[default]
    IamIdentity,
    /// Temporary credentials for this database user, through `redshift:GetClusterCredentials`.
    /// Provisioned clusters only.
    DbUser { db_user: String },
    /// The Data API reads this Secrets Manager secret. Only the ARN is stored; it is an
    /// identifier, not a secret.
    Secret { secret_arn: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RedshiftTableRef {
    pub schema: String,
    pub name: String,
    /// `TABLE`, `VIEW`, `EXTERNAL TABLE`, … as Redshift reports it.
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RedshiftColumn {
    pub name: String,
    pub type_name: String,
    pub nullable: bool,
    pub length: Option<i32>,
    pub precision: Option<i32>,
    pub scale: Option<i32>,
    pub default_value: Option<String>,
}
