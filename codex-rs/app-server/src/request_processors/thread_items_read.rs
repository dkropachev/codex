//! Reads selected durable thread items through the existing thread-store projection.

use super::thread_processor::THREAD_ITEMS_MAX_LIMIT;
use super::thread_processor::stored_thread_item_to_entry;
use super::*;
use crate::error_code::method_not_found;

impl ThreadRequestProcessor {
    pub(crate) async fn thread_items_read(
        &self,
        params: ThreadItemsReadParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        Self::read_thread_items(self.thread_store.as_ref(), params)
            .await
            .map(|response| Some(response.into()))
    }

    pub(super) async fn read_thread_items(
        store: &dyn ThreadStore,
        params: ThreadItemsReadParams,
    ) -> Result<ThreadItemsReadResponse, JSONRPCErrorError> {
        if !(1..=THREAD_ITEMS_MAX_LIMIT).contains(&params.item_ids.len()) {
            return Err(invalid_request(
                "itemIds must contain 1 to 100 IDs".to_string(),
            ));
        }
        let ThreadItemsReadParams {
            thread_id,
            turn_id,
            item_ids,
        } = params;
        let thread_id = ThreadId::from_string(&thread_id)
            .map_err(|err| invalid_request(format!("invalid thread id: {err}")))?;
        let page_size = item_ids.len();
        let page = store
            .list_items(StoreListItemsParams {
                item_ids: Some(item_ids),
                thread_id,
                turn_id: Some(turn_id),
                include_archived: true,
                position: None,
                page_size,
                sort_direction: StoreSortDirection::Asc,
                sort_key: StoreItemSortKey::CreatedAtOrdinal,
                after_updated_at_ordinal: None,
            })
            .await
            .map_err(|err| match err {
                ThreadStoreError::InvalidRequest { message } => invalid_request(message),
                ThreadStoreError::Unsupported { .. } => {
                    method_not_found("thread/items/read is not supported yet")
                }
                ThreadStoreError::ThreadNotFound { thread_id } => {
                    invalid_request(format!("no rollout found for thread id {thread_id}"))
                }
                err => internal_error(format!("failed to read thread items: {err}")),
            })?;
        let data = page
            .items
            .into_iter()
            .map(stored_thread_item_to_entry)
            .filter_map(Result::transpose)
            .collect::<Result<Vec<_>, _>>()?;

        Ok(ThreadItemsReadResponse { data })
    }
}
