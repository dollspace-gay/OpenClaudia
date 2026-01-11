#!/usr/bin/env python3
"""
Generative Art: Sacred Geometry Patterns
Creates mesmerizing mathematical art using only Python's standard library.
Outputs as a text-based representation and optionally as an SVG file.
"""

import math
import random
import time
from dataclasses import dataclass
from typing import List, Tuple

@dataclass
class Point:
    x: float
    y: float

class GenerativeArtist:
    def __init__(self, width: int = 100, height: int = 40):
        self.width = width
        self.height = height
        self.canvas = [[' ' for _ in range(width)] for _ in range(height)]
        self.center = Point(width / 2, height / 2)
    
    def draw_line(self, p1: Point, p2: Point, char: str = '*'):
        """Draw a line using Bresenham's algorithm."""
        x1, y1 = int(p1.x), int(p1.y)
        x2, y2 = int(p2.x), int(p2.y)
        
        dx = abs(x2 - x1)
        dy = abs(y2 - y1)
        sx = 1 if x1 < x2 else -1
        sy = 1 if y1 < y2 else -1
        err = dx - dy
        
        while True:
            if 0 <= x1 < self.width and 0 <= y1 < self.height:
                if self.canvas[y1][x1] == ' ':
                    self.canvas[y1][x1] = char
            
            if x1 == x2 and y1 == y2:
                break
            
            e2 = 2 * err
            if e2 > -dy:
                err -= dy
                x1 += sx
            if e2 < dx:
                err += dx
                y1 += sy
    
    def draw_circle(self, center: Point, radius: float, char: str = '*'):
        """Draw a circle using points."""
        for angle in range(0, 360, 2):
            rad = math.radians(angle)
            x = center.x + math.cos(rad) * radius
            y = center.y + math.sin(rad) * radius
            if 0 <= int(x) < self.width and 0 <= int(y) < self.height:
                self.canvas[int(y)][int(x)] = char
    
    def sacred_geometry_flower(self):
        """Create a Flower of Life pattern."""
        chars = ['*', '+', 'o', 'O', '@', '#']
        
        # Central circle
        self.draw_circle(self.center, 8, chars[0])
        
        # Six surrounding circles (hexagonal arrangement)
        for i in range(6):
            angle = math.radians(i * 60)
            cx = self.center.x + math.cos(angle) * 8
            cy = self.center.y + math.sin(angle) * 8
            self.draw_circle(Point(cx, cy), 8, chars[(i + 1) % len(chars)])
        
        # Second layer
        for i in range(6):
            angle = math.radians(i * 60)
            cx = self.center.x + math.cos(angle) * 16
            cy = self.center.y + math.sin(angle) * 16
            self.draw_circle(Point(cx, cy), 8, chars[(i + 3) % len(chars)])
    
    def fibonacci_spiral(self):
        """Draw a Fibonacci spiral pattern."""
        chars = ['.', ':', '-', '=', '+', '*', '#', '@']
        golden_ratio = (1 + math.sqrt(5)) / 2
        
        prev_x, prev_y = self.center.x, self.center.y
        
        for i in range(144):
            angle = i * math.pi / 180 * 137.508  # Golden angle
            radius = 0.3 * i * 0.5
            
            x = self.center.x + math.cos(angle) * radius
            y = self.center.y + math.sin(angle) * radius * 0.4  # Aspect ratio
            
            if 0 <= x < self.width and 0 <= y < self.height:
                char = chars[i % len(chars)]
                self.canvas[int(y)][int(x)] = char
                
                # Connect to previous point
                self.draw_line(Point(prev_x, prev_y), Point(x, y), char)
                prev_x, prev_y = x, y
    
    def lissajous_curve(self, a: int = 3, b: int = 4, delta: float = math.pi/2):
        """Draw a Lissajous curve."""
        prev_x, prev_y = None, None
        chars = ['.', '+', '*']
        
        for t in range(0, 1000):
            angle = t * 2 * math.pi / 1000
            x = self.center.x + (self.width / 3) * math.sin(a * angle + delta)
            y = self.center.y + (self.height / 3) * math.sin(b * angle)
            
            if 0 <= x < self.width and 0 <= y < self.height:
                char = chars[t % len(chars)]
                self.canvas[int(y)][int(x)] = char
                
                if prev_x is not None:
                    self.draw_line(Point(prev_x, prev_y), Point(x, y), char)
                prev_x, prev_y = x, y
    
    def phyllotaxis_mandala(self):
        """Create a phyllotaxis (leaf arrangement) mandala."""
        points = []
        num_points = 500
        angle_step = 137.5  # Golden angle in degrees
        
        for i in range(num_points):
            angle = math.radians(i * angle_step)
            radius = 0.4 * math.sqrt(i)
            
            x = self.center.x + math.cos(angle) * radius
            y = self.center.y + math.sin(angle) * radius
            
            points.append((x, y))
            
            if 0 <= int(x) < self.width and 0 <= int(y) < self.height:
                chars = ['.', ':', ';', '+', '*', 'o', 'O', '@']
                self.canvas[int(y)][int(x)] = chars[i % len(chars)]
        
        # Connect nearby points
        for i in range(len(points)):
            for j in range(i + 1, min(i + 50, len(points))):
                x1, y1 = points[i]
                x2, y2 = points[j]
                dist = math.sqrt((x2-x1)**2 + (y2-y1)**2)
                if dist < 3:
                    self.draw_line(Point(x1, y1), Point(x2, y2), '.')
    
    def random_walk(self, steps: int = 3000):
        """Generate art through random walk."""
        x, y = self.center.x, self.center.y
        chars = ['.', ',', '-', '~', ':', ';', '=', '+', '*', '#', '@']
        
        for i in range(steps):
            angle = random.random() * 2 * math.pi
            step_size = 0.5 + random.random() * 1.5
            
            new_x = x + math.cos(angle) * step_size
            new_y = y + math.sin(angle) * step_size * 0.5
            
            if 0 <= int(new_x) < self.width and 0 <= int(new_y) < self.height:
                char_idx = min(int(i / steps * len(chars)), len(chars) - 1)
                self.canvas[int(new_y)][int(new_x)] = chars[char_idx]
                self.draw_line(Point(x, y), Point(new_x, new_y), chars[char_idx])
                
                x, y = new_x, y
    
    def render(self) -> str:
        """Render canvas to string."""
        return '\n'.join(''.join(row) for row in self.canvas)
    
    def clear(self):
        """Clear the canvas."""
        self.canvas = [[' ' for _ in range(self.width)] for _ in range(self.height)]

    def save_svg(self, filename: str, title: str):
        """Save the current canvas as an SVG file."""
        with open(filename, 'w') as f:
            f.write('<?xml version="1.0" encoding="UTF-8"?>\n')
            f.write(f'<svg xmlns="http://www.w3.org/2000/svg" width="{self.width*10}" height="{self.height*10}">\n')
            f.write(f'<rect width="100%" height="100%" fill="black"/>\n')
            f.write(f'<text x="50%" y="30" text-anchor="middle" fill="white" font-size="24">{title}</text>\n')
            
            for y in range(self.height):
                for x in range(self.width):
                    char = self.canvas[y][x]
                    if char != ' ':
                        colors = {
                            '*': '#FF6B6B', '+': '#4ECDC4', 'o': '#FFE66D',
                            'O': '#FF8C42', '@': '#F72585', '#': '#7209B7',
                            '.': '#3A86FF', ':': '#8338EC', '-': '#FF006E'
                        }
                        color = colors.get(char, '#FFFFFF')
                        f.write(f'<circle cx="{x*10+5}" cy="{y*10+50}" r="3" fill="{color}"/>\n')
            
            f.write('</svg>\n')


def generate_gallery():
    """Generate a gallery of generative art pieces."""
    print("=" * 100)
    print(" " * 30 + "GENERATIVE ART GALLERY")
    print("=" * 100)
    print("\n")
    
    artist = GenerativeArtist(100, 35)
    
    # Piece 1: Sacred Geometry Flower of Life
    print("  [1] Flower of Life - Sacred Geometry")
    print("  " + "─" * 80)
    artist.sacred_geometry_flower()
    print(artist.render())
    print("\n")
    
    artist.clear()
    
    # Piece 2: Fibonacci Spiral
    print("  [2] Fibonacci Spiral - Nature's Golden Ratio")
    print("  " + "─" * 80)
    artist.fibonacci_spiral()
    print(artist.render())
    print("\n")
    
    artist.clear()
    
    # Piece 3: Lissajous Curves
    print("  [3] Lissajous Curve - Harmonic Motion Pattern")
    print("  " + "─" * 80)
    artist.lissajous_curve(5, 4, math.pi/4)
    print(artist.render())
    print("\n")
    
    artist.clear()
    
    # Piece 4: Phyllotaxis Mandala
    print("  [4] Phyllotaxis Mandala - Sunflower Pattern")
    print("  " + "─" * 80)
    artist.phyllotaxis_mandala()
    print(artist.render())
    print("\n")
    
    artist.clear()
    
    # Piece 5: Random Walk Art
    print("  [5] Random Walk - Stochastic Art")
    print("  " + "─" * 80)
    artist.random_walk(2500)
    print(artist.render())
    print("\n")
    
    # Generate SVG files
    print("\n" + "=" * 100)
    print("  Generating SVG files...")
    print("=" * 100)
    
    artist2 = GenerativeArtist(80, 80)
    
    artist2.sacred_geometry_flower()
    artist2.save_svg("flower_of_life.svg", "Flower of Life")
    print("  [*] Saved: flower_of_life.svg")
    
    artist2.clear()
    artist2.fibonacci_spiral()
    artist2.save_svg("fibonacci_spiral.svg", "Fibonacci Spiral")
    print("  [*] Saved: fibonacci_spiral.svg")
    
    artist2.clear()
    artist2.lissajous_curve(7, 6, 0)
    artist2.save_svg("lissajous.svg", "Lissajous Curve")
    print("  [*] Saved: lissajous.svg")
    
    artist2.clear()
    artist2.phyllotaxis_mandala()
    artist2.save_svg("phyllotaxis.svg", "Phyllotaxis Mandala")
    print("  [*] Saved: phyllotaxis.svg")
    
    print("\n" + "=" * 100)
    print("  All art pieces generated successfully!")
    print("  SVG files can be opened in any web browser or vector graphics editor.")
    print("=" * 100)


def main():
    """Main entry point."""
    start_time = time.time()
    
    try:
        generate_gallery()
    except KeyboardInterrupt:
        print("\n\n[!] Interrupted by user")
    except Exception as e:
        print(f"\n[!] Error: {e}")
        raise
    
    elapsed = time.time() - start_time
    print(f"\n  Generated in {elapsed:.2f} seconds\n")


if __name__ == "__main__":
    main()
