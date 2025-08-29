# Fix for LLM Response Display Issues

## Problem
The LLM response display was breaking in certain cases, particularly when:
1. Responses contained line breaks that weren't properly handled
2. The display area calculation didn't account for wrapped lines correctly
3. ANSI escape sequences in responses could break raw mode terminal rendering

## Root Causes Identified
1. **Line Wrapping Issues**: The `wrap_display` function wasn't properly handling newline characters, causing display issues when content exceeded the visible area.
2. **Scroll State Management**: When new content was added during streaming, the scroll state wasn't properly maintained.
3. **ANSI Sequence Handling**: While the code did sanitize responses, there were edge cases where terminal control sequences could still break the display.

## Changes Made

### 1. Fixed Line Wrapping (`src/tui/state_render.rs`)
- Enhanced the `wrap_display` function to explicitly handle newline characters
- Improved the logic for wrapping lines when they exceed the maximum width
- Ensured that empty lines are properly handled in the wrapping process

### 2. Improved Scroll State Management (`src/tui/state.rs`)
- Maintained consistent scroll state handling when adding new content
- Ensured that new messages are properly counted when not in auto-scroll mode
- Fixed the logic for scrolling to bottom when new content is added

### 3. Enhanced LLM Response Sanitization (`src/tui/llm_response_handler.rs`)
- Improved the `sanitize_for_display` function to better handle ANSI escape sequences
- Ensured proper handling of carriage returns and other control characters
- Maintained better consistency in how streaming tokens are processed and displayed

### 4. Improved Rendering Logic (`src/tui/rendering.rs`)
- Enhanced debug logging to better track line rendering
- Maintained proper styling for different types of content (code blocks, LLM responses, etc.)

## Testing
All existing tests continue to pass, confirming that our changes don't break existing functionality while fixing the display issues.

## Verification
To verify the fix:
1. Start the application
2. Send a prompt that would generate a long response with multiple line breaks
3. Verify that the response is displayed correctly without breaking the terminal display
4. Test scrolling through the response using Ctrl+Up/Down keys
5. Confirm that ANSI sequences in responses don't break the terminal