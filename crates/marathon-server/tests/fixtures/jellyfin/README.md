Jellyfin API responses for `tests/jellyfin.rs`, in the shape Jellyfin
10.10 returns them (trimmed to a few items, with made-up IDs). The mock
server in the test serves them, applying `ParentId`, `IncludeItemTypes`,
`ExcludeLocationTypes`, and paging itself.

- `virtual_folders.json`: `GET /Library/VirtualFolders`
- `items_shows.json`, `items_films.json`: everything below each library
  (what `GET /Items?ParentId=…&Recursive=true` draws from)
