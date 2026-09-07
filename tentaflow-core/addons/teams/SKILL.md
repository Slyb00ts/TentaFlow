# Microsoft Teams

Microsoft Teams addon provides access to chats, channels, calendar, files and meetings via Microsoft Graph API.

## Tools

### teams.send_message
Send a message to a Teams user or channel.

When to use:
- User wants to send a quick message on Teams
- User asks to post to a channel or chat with someone
- Informal, quick communication context

TOON examples:
- `@teams.send_message|to=jan@company.com|message=Hi, meeting at 3PM`
- `@teams.send_message|to=jan@company.com|message=OK|channel_id=19:abc...`
- `#MISSING@teams.send_message|to=jan|message=?`

Parameters:
- **to*** — user email or channel ID
- **message*** — message content
- **channel_id** — channel ID (optional, for channel posts)

### teams.list_messages
Get recent messages from a chat or channel.

When to use:
- User wants to see chat history
- User asks what was posted in a channel
- User needs context from a conversation

TOON examples:
- `@teams.list_messages|chat_id=19:abc...`
- `@teams.list_messages|chat_id=19:abc...|limit=50`

Parameters:
- **chat_id*** — chat or channel ID
- **limit** — number of messages (default: 20)

### teams.list_chats
List user's active chats.

When to use:
- User asks to see their chats
- User wants to find a conversation
- User needs a chat ID for list_messages

TOON examples:
- `@teams.list_chats`

Parameters: none required

### teams.list_channels
List channels in a team.

When to use:
- User asks about available channels in a team
- User wants to post to a specific channel

TOON examples:
- `@teams.list_channels|team_id=abc-123`

Parameters:
- **team_id*** — team ID

### teams.get_calendar
Get calendar events and meetings.

When to use:
- User asks about their schedule, upcoming meetings
- User wants to know what's on their calendar

TOON examples:
- `@teams.get_calendar`
- `@teams.get_calendar|days=3`

Parameters:
- **days** — number of days ahead (default: 7)

### teams.list_files
List files from OneDrive/SharePoint.

When to use:
- User asks about files shared in Teams
- User wants to browse project folders

TOON examples:
- `@teams.list_files`
- `@teams.list_files|path=/Projects/Alpha`

Parameters:
- **path** — file path (default: root)

### teams.join_meeting
Join a Teams meeting as a bot.

When to use:
- User asks to join a meeting with the bot
- User wants the bot to attend and take notes

TOON examples:
- `@teams.join_meeting|meeting_id=abc-123`

Parameters:
- **meeting_id*** — meeting ID

### teams.get_meeting_notes
Get meeting notes and transcription.

When to use:
- User asks for meeting notes or summary
- User wants to know what was discussed
- User needs a meeting transcription

TOON examples:
- `@teams.get_meeting_notes|meeting_id=abc-123`

Parameters:
- **meeting_id*** — meeting ID

## Scenarios

### Check chats and reply
```toon
@teams.list_chats
@teams.list_messages|chat_id={result}
@teams.send_message|to={user}|message=Thanks, confirmed.
```

### Check calendar and join meeting
```toon
@teams.get_calendar|days=1
@teams.join_meeting|meeting_id={result}
@teams.get_meeting_notes|meeting_id={result}
```

### Browse team files
```toon
@teams.list_channels|team_id={team}
@teams.list_files|path=/General
```

## Notes
- User must configure OAuth in addon settings before using Teams tools
- Bot uses STT/TTS from the router for understanding and responding in meetings
- Meeting notes are generated automatically by LLM from transcription
- For sending emails (formal messages with subject), use the Outlook addon instead
