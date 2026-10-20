//! Reads selected durable thread items through the existing thread-store projection.

use super::thread_processor::stored_thread_item_to_entry;
use super::*;
use crate::error_code::method_not_found;

impl ThreadRequestProcessor {
    pub(crate) async fn thread_items_read(
        &self,
        params: ThreadItemsReadParams,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        self.thread_items_read_response_inner(params)
            .await
            .map(|response| Some(response.into()))
    }

    async fn thread_items_read_response_inner(
        &self,
        params: ThreadItemsReadParams,
    ) -> Result<ThreadItemsReadResponse, JSONRPCErrorError> {
        if !(1..=100).contains(&params.item_ids.len()) {
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
        let page = self
            .thread_store
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
