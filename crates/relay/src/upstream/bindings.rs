//! Only ownership and routing metadata are durable; upstream response content is not.
use crate::{
    error::{Error, Result},
    operations::{ClientAccess, Operations},
    provider::Provider,
};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Binding {
    pub account: Uuid,
    pub model: String,
    pub provider: Provider,
    pub websocket: bool,
    #[serde(default)]
    pub native_tools: bool,
}
fn storage(_: rusqlite::Error) -> Error {
    Error::new("RESPONSE_STORAGE", "上游响应绑定存储失败。")
}
pub(super) fn lookup(ops: &Operations, access: &ClientAccess, id: &str) -> Result<Option<Binding>> {
    lookup_scoped(ops, access, id, "upstream")
}
fn lookup_scoped(
    ops: &Operations,
    access: &ClientAccess,
    id: &str,
    scope: &str,
) -> Result<Option<Binding>> {
    ops.revalidate_api_key(access)?;
    ops.with_response_db(|db| {
        let text: Option<String> = db.query_row("SELECT binding FROM response_affinities WHERE owner=?1 AND group_id=?2 AND fingerprint=?3 AND expires>?4",params![access.key_id.to_string(),access.group_id.to_string(),format!("{scope}:{id}"),crate::now_ms() as i64],|r|r.get(0)).optional().map_err(storage)?;
        let binding: Option<Binding> = text.map(|s|serde_json::from_str(&s)).transpose()?;
        if binding.as_ref().is_some_and(|b| !access.allows(&b.model)) { return Err(Error::new("MODEL_NOT_ALLOWED","此密钥未授权该响应的模型。")); }
        Ok(binding)
    })
}
pub(super) fn save(
    ops: &Operations,
    access: &ClientAccess,
    id: &str,
    binding: &Binding,
) -> Result<()> {
    match save_scoped(ops, access, id, binding, "upstream") {
        Err(error) if error.code == "RESPONSE_STORAGE_FULL" => Ok(()),
        result => result,
    }
}
fn save_scoped(
    ops: &Operations,
    access: &ClientAccess,
    id: &str,
    binding: &Binding,
    scope: &str,
) -> Result<()> {
    if id.is_empty()
        || id.len() > 512
        || matches!(id, "." | "..")
        || !id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._:-".contains(&c))
    {
        return Err(Error::new(
            "UPSTREAM_INVALID_RESPONSE",
            "上游响应标识无效。",
        ));
    }
    ops.with_response_db(|db| {
        let tx=db.transaction().map_err(storage)?;
        tx.execute("DELETE FROM response_affinities WHERE expires<=?1",[crate::now_ms() as i64]).map_err(storage)?;
        let existing:Option<String>=tx.query_row("SELECT binding FROM response_affinities WHERE owner=?1 AND group_id=?2 AND fingerprint=?3",params![access.key_id.to_string(),access.group_id.to_string(),format!("{scope}:{id}")],|r|r.get(0)).optional().map_err(storage)?;
        if let Some(existing)=existing {
            let old:Binding=serde_json::from_str(&existing)?;
            if old.account!=binding.account || old.model!=binding.model || old.provider!=binding.provider || (scope=="search" && old.native_tools!=binding.native_tools) {
                return Err(Error::new("RESPONSE_CHAIN_MISMATCH","上游返回了已属于其他账户或配置的响应标识。"));
            }
        }
        tx.execute("INSERT INTO response_affinities(owner,group_id,model,fingerprint,account,binding,expires) VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(owner,group_id,model,fingerprint) DO UPDATE SET binding=excluded.binding,account=excluded.account,expires=excluded.expires",params![access.key_id.to_string(),access.group_id.to_string(),binding.model,format!("{scope}:{id}"),binding.account.to_string(),serde_json::to_string(binding)?,(crate::now_ms()+30*86_400_000) as i64]).map_err(storage)?;
        let bytes: i64=tx.query_row("SELECT COALESCE(SUM(length(binding)+length(fingerprint)+160),0) FROM response_affinities",[],|r|r.get(0)).map_err(storage)?;
        let budget=std::env::var("BRIDGE_RESPONSE_BINDING_BYTES").ok().and_then(|v|v.parse::<i64>().ok()).unwrap_or(1024*1024*1024);
        if bytes>budget { return Err(Error::new("RESPONSE_STORAGE_FULL","账户绑定存储容量已满。")); }
        tx.commit().map_err(storage)
    })
}
pub(crate) fn search_account(
    ops: &Operations,
    access: &ClientAccess,
    id: &str,
    model: &str,
) -> Result<Option<Uuid>> {
    let binding = lookup_scoped(ops, access, id, "search")?;
    if let Some(binding) = &binding {
        if binding.model != model {
            return Err(Error::new(
                "RESPONSE_CHAIN_MISMATCH",
                "更换搜索模型须使用新的搜索会话 ID。",
            ));
        }
        ops.check_inference_route(binding.account, access, &binding.provider, model)?;
    }
    Ok(binding.map(|b| b.account))
}
pub(crate) fn bind_search(
    ops: &Operations,
    access: &ClientAccess,
    id: &str,
    account: Uuid,
    model: &str,
    provider: &Provider,
    native_tools: bool,
) -> Result<()> {
    // 首次发送前原子绑定；并发首请求选中不同账号时拒绝后者，避免污染搜索引用。
    // 独立前缀与 Key/分组隔离，既不存查询内容，也不会与 Responses ID 冲突。
    save_scoped(
        ops,
        access,
        id,
        &Binding {
            account,
            model: model.into(),
            provider: provider.clone(),
            websocket: false,
            native_tools,
        },
        "search",
    )
}
pub(crate) fn search_native_mode(
    ops: &Operations,
    access: &ClientAccess,
    id: &str,
) -> Result<Option<bool>> {
    Ok(lookup_scoped(ops, access, id, "search")?.map(|b| b.native_tools))
}
pub(super) fn delete(ops: &Operations, access: &ClientAccess, id: &str) -> Result<()> {
    ops.with_response_db(|db| {
        db.execute(
            "DELETE FROM response_affinities WHERE owner=?1 AND group_id=?2 AND fingerprint=?3",
            params![
                access.key_id.to_string(),
                access.group_id.to_string(),
                format!("upstream:{id}")
            ],
        )
        .map_err(storage)?;
        Ok(())
    })
}
