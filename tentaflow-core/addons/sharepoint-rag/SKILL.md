# SharePoint RAG

SharePoint RAG addon indexes and searches SharePoint files for Retrieval-Augmented Generation.

## Tools

### sharepoint_rag.list_sites
List configured SharePoint sites with their metadata.

When to use:
- User asks what SharePoint sites are available
- User wants to browse sites before searching

TOON examples:
- `@sharepoint_rag.list_sites`

Parameters: none required

### sharepoint_rag.list_files
List files in a specific site or folder.

When to use:
- User wants to browse files on SharePoint
- User asks what's in a specific folder
- User needs to find a file by browsing

TOON examples:
- `@sharepoint_rag.list_files`
- `@sharepoint_rag.list_files|site_url=https://company.sharepoint.com/sites/HR|path=/Documents`
- `@sharepoint_rag.list_files|recursive=true`

Parameters:
- **site_url** — site URL (default: first configured)
- **path** — folder path (default: root)
- **recursive** — list recursively (default: false)

### sharepoint_rag.search_files
Search files by name or content.

When to use:
- User wants to find a document about a specific topic
- User searches for PDF reports or specific file types
- User asks "where is the policy document?"

TOON examples:
- `@sharepoint_rag.search_files|query=budget 2026`
- `@sharepoint_rag.search_files|query=report|file_type=pdf`
- `@sharepoint_rag.search_files|query=vacation policy|site_url=https://company.sharepoint.com/sites/HR`

Parameters:
- **query*** — search phrase
- **site_url** — limit to a specific site
- **file_type** — filter by extension (e.g. pdf, docx)

### sharepoint_rag.get_file_content
Get file content (text or metadata).

When to use:
- User wants to read a document's content
- User asks "what's in this file?"
- RAG context retrieval — use content as context for answering questions

TOON examples:
- `@sharepoint_rag.get_file_content|file_id=drive123:item456`
- `@sharepoint_rag.get_file_content|file_id=drive123:item456|format=metadata`

Parameters:
- **file_id*** — file ID (from list_files or search_files)
- **format** — text (default), metadata, raw

### sharepoint_rag.get_file_info
Get file metadata without content (size, dates, author).

When to use:
- User asks who edited a file, when it was modified, or file size
- User needs file properties without downloading content

TOON examples:
- `@sharepoint_rag.get_file_info|file_id=drive123:item456`

Parameters:
- **file_id*** — file ID

### sharepoint_rag.list_recent_changes
List recently changed files.

When to use:
- User asks what changed on SharePoint recently
- User wants to see latest document updates

TOON examples:
- `@sharepoint_rag.list_recent_changes`
- `@sharepoint_rag.list_recent_changes|days=3`
- `@sharepoint_rag.list_recent_changes|site_url=https://company.sharepoint.com/sites/HR`

Parameters:
- **site_url** — limit to a specific site
- **days** — number of days back (default: 7)

### sharepoint_rag.sync_index
Trigger file index synchronization.

When to use:
- User says search results are outdated
- User wants to reindex files after adding new documents
- Admin requests manual sync

TOON examples:
- `@sharepoint_rag.sync_index`
- `@sharepoint_rag.sync_index|force=true`

Parameters:
- **site_url** — limit to a specific site
- **force** — full reindex (default: false — incremental only)

## Scenarios

### Typical RAG flow
```toon
@sharepoint_rag.search_files|query=vacation policy
@sharepoint_rag.get_file_content|file_id={result}|format=text
```
Use retrieved content as context for answering. Always cite the source (file name, link).

### Check recent changes
```toon
@sharepoint_rag.list_recent_changes|days=1
@sharepoint_rag.get_file_info|file_id={result}
```

### Browse a site
```toon
@sharepoint_rag.list_sites
@sharepoint_rag.list_files|site_url={result}|recursive=true
```

## Notes
- file_id format is "drive_id:item_id" — use the exact value returned by list_files/search_files
- Addon uses Application permissions — does not require user login
- Access is limited to sites configured by the administrator
- Office files (docx, pptx, xlsx) are automatically converted to text
- Large files (>50MB by default) are skipped — limit is configurable
