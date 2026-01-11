#!/usr/bin/env python3
"""
A mesmerizing terminal particle simulation.
Generates beautiful spiral patterns using ASCII characters.
"""

import math
import random
import time
import sys
import signal
from dataclasses import dataclass
from typing import List

# ANSI color codes for vibrant output
COLORS = [
    "\033[91m",  # Red
    "\033[92m",  # Green
    "\033[93m",  # Yellow
    "\033[94m",  # Blue
    "\033[95m",  # Magenta
    "\033[96m",  # Cyan
    "\033[97m",  # White
]
RESET = "\033[0m"

@dataclass
class Particle:
    x: float
    y: float
    vx: float
    vy: float
    life: int
    max_life: int
    color: str
    char: str

class SpiralSimulator:
    def __init__(self, width: int = 80, height: int = 24):
        self.width = width
        self.height = height
        self.particles: List[Particle] = []
        self.frame = 0
        self.running = True
        self.center_x = width // 2
        self.center_y = height // 2

        # Get terminal size dynamically
        try:
            import shutil
            self.width, self.height = shutil.get_terminal_size()
            self.center_x = self.width // 2
            self.center_y = height // 2
        except:
            pass

        signal.signal(signal.SIGINT, self._signal_handler)

    def _signal_handler(self, signum, frame):
        self.running = False
        print(RESET + "\nThanks for watching!")
        sys.exit(0)

    def emit_spiral(self):
        """Emit particles in a spiral pattern from the center."""
        num_particles = 12
        spiral_speed = 0.25
        
        for i in range(num_particles):
            angle = self.frame * spiral_speed + (i * 2 * math.pi / num_particles)
            radius = 1 + math.sin(self.frame * 0.02) * 0.5
            
            # Calculate spiral emission point
            emit_x = self.center_x + math.cos(angle) * radius * 3
            emit_y = self.center_y + math.sin(angle) * radius * 1.5
            
            # Velocity spirals outward
            speed = 0.6 + random.random() * 0.4
            vx = math.cos(angle) * speed
            vy = math.sin(angle) * speed * 0.5  # Compress Y for aspect ratio
            
            # Add some variation
            vx += (random.random() - 0.5) * 0.2
            vy += (random.random() - 0.5) * 0.1
            
            color = COLORS[self.frame % len(COLORS)]
            chars = ['.', '*', '+', 'o', 'O']
            char = chars[i % len(chars)]
            
            life = 60 + random.randint(0, 30)
            
            self.particles.append(Particle(
                x=emit_x, y=emit_y,
                vx=vx, vy=vy,
                life=life, max_life=life,
                color=color, char=char
            ))

    def emit_burst(self):
        """Occasionally emit radial bursts."""
        if random.random() < 0.04:
            num_particles = 15
            burst_x = self.center_x + random.randint(-8, 8)
            burst_y = self.center_y + random.randint(-4, 4)
            
            for i in range(num_particles):
                angle = (i / num_particles) * 2 * math.pi
                speed = 0.5 + random.random() * 0.6
                
                vx = math.cos(angle) * speed
                vy = math.sin(angle) * speed * 0.5
                
                color = random.choice(COLORS)
                char = random.choice(['*', '#', '@'])
                
                self.particles.append(Particle(
                    x=burst_x, y=burst_y,
                    vx=vx, vy=vy,
                    life=35 + random.randint(0, 15), max_life=50,
                    color=color, char=char
                ))

    def update(self):
        """Update all particles."""
        self.frame += 1
        
        # Emit new particles
        self.emit_spiral()
        self.emit_burst()
        
        # Update existing particles
        for p in self.particles:
            p.x += p.vx
            p.y += p.vy
            
            # Add gentle gravity
            p.vy += 0.015
            
            # Slow down slightly (friction)
            p.vx *= 0.99
            p.vy *= 0.99
            
            # Decrease life
            p.life -= 1
            
            # Fade character based on life
            life_ratio = p.life / p.max_life
            if life_ratio < 0.25:
                p.char = '.'
            elif life_ratio < 0.5:
                p.char = '+'
            elif life_ratio < 0.75:
                p.char = 'o'
        
        # Remove dead particles
        self.particles = [p for p in self.particles 
                         if p.life > 0 and 0 <= p.x < self.width and 0 <= p.y < self.height]

    def render(self) -> str:
        """Render the particle system to a string."""
        # Create a grid buffer
        grid = [[' ' for _ in range(self.width)] for _ in range(self.height)]
        color_grid = [[None for _ in range(self.width)] for _ in range(self.height)]
        
        # Place particles on grid
        for p in self.particles:
            ix, iy = int(p.x), int(p.y)
            if 0 <= ix < self.width and 0 <= iy < self.height:
                grid[iy][ix] = p.char
                color_grid[iy][ix] = p.color
        
        # Build the output string
        lines = []
        for y in range(self.height):
            line = ""
            for x in range(self.width):
                if color_grid[y][x]:
                    line += color_grid[y][x] + grid[y][x] + RESET
                else:
                    line += grid[y][x]
            lines.append(line)
        
        return '\n'.join(lines)

    def run(self, fps: int = 30):
        """Run the simulation."""
        frame_time = 1.0 / fps
        
        # Clear screen and position cursor
        print("\033[2J\033[H", end="")
        print("=== Spiral Particle Simulation ===", end="", flush=True)
        time.sleep(0.5)
        
        print("\033[?25l", end="")  # Hide cursor
        
        try:
            while self.running:
                start_time = time.time()
                
                self.update()
                output = self.render()
                
                # Move cursor to top-left and render
                print(f"\033[1;1H{output}", end="", flush=True)
                
                # Maintain framerate
                elapsed = time.time() - start_time
                sleep_time = frame_time - elapsed
                if sleep_time > 0:
                    time.sleep(sleep_time)
                    
        finally:
            print(RESET + "\033[?25h")  # Show cursor and reset colors
            print("\n" + "=" * 40)
            print(f"Total frames rendered: {self.frame}")
            print(f"Final particles: {len(self.particles)}")
            print("=" * 40)


def main():
    """Run the spiral particle simulation."""
    print("\n*** Spiral Particle Simulation ***")
    print("Press Ctrl+C to exit\n")
    time.sleep(0.8)
    
    sim = SpiralSimulator()
    sim.run(fps=25)


if __name__ == "__main__":
    main()
