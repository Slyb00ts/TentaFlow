# Outlook

Microsoft Outlook addon provides email access via Microsoft Graph API.

## Tools

### outlook.list_emails
List emails from a mail folder.

When to use:
- User wants to see recent emails, check inbox, view unread messages
- User asks about mail in a specific folder (sent, drafts, deleted, junk, archive)

TOON examples:
- `@outlook.list_emails`
- `@outlook.list_emails|folder=sent|limit=10`
- `@outlook.list_emails|filter=isRead eq false`

Parameters:
- **folder** — inbox (default), sent, drafts, deleted, junk, archive
- **limit** — number of emails (default: 20, max: 50)
- **skip** — pagination offset
- **filter** — OData filter (e.g. "isRead eq false", "hasAttachments eq true")

### outlook.read_email
Read a specific email with full content and attachments.

When to use:
- User wants to read full email content
- User needs to see attachments or email details

TOON examples:
- `@outlook.read_email|message_id=AAMk...`

Parameters:
- **message_id*** — email message ID (from list_emails or search_emails)

### outlook.search_emails
Search emails by content, subject or sender.

When to use:
- User wants to find emails about a specific topic
- User searches by sender, date range, or attachment presence

TOON examples:
- `@outlook.search_emails|query=invoice|has_attachment=true`
- `@outlook.search_emails|query=project Alpha|from_date=2026-03-01`

Parameters:
- **query*** — search phrase
- **folder** — folder to search in (default: all)
- **from_date** — start date in ISO 8601 format
- **has_attachment** — filter only emails with attachments

### outlook.send_email
Send a new email message.

When to use:
- User wants to send an email, compose a message, write to someone via email
- User mentions subject line, CC, attachments, formal message

TOON examples:
- `@outlook.send_email|to=jan@company.com|subject=Report|body=Please find the quarterly report attached.`
- `@outlook.send_email|to=anna@company.com|subject=Meeting|body=Rescheduled to 3PM.|cc=mark@company.com`
- `#MISSING@outlook.send_email|to=kate|subject=?|body=?`

Parameters:
- **to*** — recipient email address (or comma-separated list)
- **subject*** — email subject
- **body*** — email body text
- **cc** — CC addresses (comma-separated)
- **is_html** — HTML body format (default: false)

Note: ALWAYS confirm content, subject and recipient with user before sending.

### outlook.reply_email
Reply to an email message.

When to use:
- User wants to reply to an email
- User says "respond to this", "reply with thanks"

TOON examples:
- `@outlook.reply_email|message_id=AAMk...|body=Thank you, confirmed.`

Parameters:
- **message_id*** — ID of the message to reply to
- **body*** — reply body text

Note: ALWAYS confirm reply content with user before sending.

### outlook.list_folders
List user's mail folders.

When to use:
- User asks about available folders
- User wants to know unread email count per folder

TOON examples:
- `@outlook.list_folders`

Parameters: none required

### outlook.get_attachment
Download an attachment from an email message.

When to use:
- User wants to download or view an attachment
- User asks for a specific file from an email

TOON examples:
- `@outlook.get_attachment|message_id=AAMk...|attachment_id=AAMk...`

Parameters:
- **message_id*** — email message ID
- **attachment_id*** — attachment ID (from read_email)

## Scenarios

### Check inbox and reply
```toon
@outlook.list_emails|filter=isRead eq false
@outlook.read_email|message_id={result}
@outlook.reply_email|message_id={result}|body=Thank you, confirmed.
```

### Find and forward
```toon
@outlook.search_emails|query=quarterly report
@outlook.read_email|message_id={result}
@outlook.send_email|to=boss@company.com|subject=Fwd: Quarterly report|body=Forwarding the report.
```

### Download attachment
```toon
@outlook.read_email|message_id={result}
@outlook.get_attachment|message_id={result}|attachment_id={from_read_email}
```

## Notes
- Always use list_emails or search_emails before read_email — you need the message_id
- Never send an email without explicit user confirmation
- If user is not logged in, inform about OAuth login requirement
- Email preview is limited to 200 chars — use read_email for full content
