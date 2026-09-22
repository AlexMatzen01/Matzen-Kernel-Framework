--[[
    MFK Desktop Default Theme
    
    All colors are 24-bit RGB hex values (0xRRGGBB).
    Font sizes are in pixels.
    Metrics are in pixels unless otherwise noted.
--]]

return {
    colors = {
        -- Base colors
        bg = 0x102a4e,           -- Dark blue background
        panel_bg = 0x1c4476,     -- Slightly lighter panel background
        panel_border = 0xebebeb, -- Light gray border
        accent = 0x00be5a,       -- Green accent color
        
        -- Window titlebar
        title_active = 0x005cac,   -- Active window titlebar
        title_inactive = 0x696969, -- Inactive window titlebar
        
        -- Text
        text = 0xffffff,       -- Primary text (white)
        text_muted = 0xaaaaaa, -- Muted/disabled text
        
        -- Buttons
        button_bg = 0x005cac,      -- Default button background
        button_hover = 0x0078d4,   -- Button hover state
        button_press = 0x003d7a,   -- Button pressed state
        button_text = 0xffffff,    -- Button text color
        
        -- Cursor
        cursor = 0xffffffff,       -- White cursor
        
        -- Scrollbar
        scrollbar_bg = 0x1c4476,
        scrollbar_thumb = 0x696969,
        scrollbar_hover = 0xaaaaaa,
    },
    
    fonts = {
        ui = { path = "fonts/ibm-plex-mono-regular.ttf", size = 14 },
        mono = { path = "fonts/ibm-plex-mono-regular.ttf", size = 12 },
        title = { path = "fonts/ibm-plex-sans-bold.ttf", size = 16 },
    },
    
    metrics = {
        window_border = 2,
        titlebar_height = 28,
        button_padding_x = 16,
        button_padding_y = 8,
        panel_radius = 4,
        taskbar_height = 36,
        icon_size = 48,
    },
    
    layout = {
        icon_spacing = 16,
        icon_margin = 16,
        window_gap = 8,
    },
}